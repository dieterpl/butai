# The butai kit

**Read this before adding or changing a component in `src/components/`.**

`src/components/ui/` is **real shadcn**, added by its CLI. `src/components/` is
butai's own vocabulary — the shapes shadcn has no opinion about because they are
about *this* app: a selectable row, a panel frame, a meter with a track, an
elided path.

## What this kit looks like, and where that came from

**It looks like the terminal client, and that is not a metaphor.**
`docs/images/workbench.svg`, `settings.svg` and `help.svg` are **real captures**
— the TUI run in a pty against a throwaway repo, with the rendered cell grid
written out as SVG. Every `<rect fill>` and every `<text>` run in them is
exactly what the terminal drew. They are the specification for this directory.

Read one before you change a shape here. `python3 -c` over the `<text>` runs
reconstructs the grid in about fifteen lines and is worth more than any
description of it, this one included.

```
┌ AGENTS ─────────[+ agent]┐┌ STAGE · claude ──────┐┌ CHANGES (5) · berth-clearance ─┐
│  gemini              WAIT││                      ││Unstaged                        │
│  codex              ⠇ 15s││● Read(src/berth.rs)  ││  M src/berth.rs          +10 -5│
│> claude             done•││  ⎿  Read 24 lines    ││  M src/lib.rs             +1 -0│
│a new... · x kill         ││                      ││c commit · g git · ? keys       │
└──────────────────────────┘└──────────────────────┘└────────────────────────────────┘
```

**This reverses an earlier pass.** That one moved the client to a *web app*
look — system sans for the chrome, 32px rows, four radii, four shadows, a
one-pixel press, 150ms motion — and it has been undone deliberately and in full.
If you find a comment in here still arguing for it, that comment is stale; the
file's git history has the argument, and it lost. Do not reintroduce half of it
as a compromise.

## The rules

**One family.** `--font-sans` and `--font-mono` are the same monospace stack.
There is no second family to reach for. The names both survive because a page
saying `font-mono` about a path is making a claim about *why* — that the string
is compared character by character — and that claim is worth keeping even while
the two resolve to the same thing.

**One size.** 14px on an 18px line: the terminal's cell. The `text-11` … `16`
names survive as aliases onto two steps — 14px for anything that is a line of
the workbench, 12px for an annotation the TUI would not have drawn at all.

**Nothing is raised and nothing is rounded.** The `--radius-*` and `--shadow-*`
namespaces are *cleared* in `styles.css`, so `rounded-md` and `shadow-sm`
generate no CSS anywhere in the client, including inside shadcn's own source.
Do not add them back. A corner is `┌`, which is two strokes meeting.

**Nothing moves.** `--default-transition-duration` is `0s`. A terminal
repaints.

**A panel is a frame with its title let into the top rule.** `Card` draws the
box; `SectionTitle` draws the top of it. They are two halves of one shape — read
both doc comments before touching either, because the mechanism (an *inset
shadow*, not a `border`) is forced by every list card setting `overflow-hidden`,
and it is not obvious from either file alone.

**A button is a word in square brackets.** `[+ agent]`, `[+ term]`, `[open]`.
The variants pick a pen, not a fill — except `default`, which is the accent band
the TUI paints behind the thing you are looking at.

**Selection is a background band.** One concept, one colour, no ring. See `Row`.

## The scale, fixed

Everything comes from `src/styles.css`'s `@theme`. Do not invent a value.

```
spacing   Tailwind's 1/2/3/4 — which is 4/8/12/16. `p-1.5` is the smell.
radius    none. The namespace is cleared; `rounded-none` is the only spelling.
shadow    none. Same. `shadow-[inset_0_0_0_1px_var(--color-border)]` is a frame,
          not an elevation, and it is the one exception.
type      text-13 (the cell, 14px) · text-11 (an annotation, 12px). Both on 18px.
rows      h-row 22px (default) · h-row-compact 20px (dense lists) · h-row-lg 26px
motion    none
```

**22px, not 18px, is the one deliberate concession to the pointer.** The
terminal's line is 18px at 14px type. Four pixels buys a row you can click at
without it reading as a web app's control. It is spent once, here, and nowhere
else — do not spend it again on a taller button or a padded card.

## The two rules that are not about geometry

**Mono is now everything, and `font-mono` is still a claim.** Put it on anything
you would compare character by character — a path, a diff, a sha, a pane's
screen, a number in a column. It changes no pixels today. It says which strings
would break if the two families ever diverged again, which is the only reason
the distinction is still written down.

**No colour literal, anywhere.** Every colour is a token from `@theme`, which is
`var(--x)`, which is `settings.ts`'s palette. A `#`, `rgb(`, `hsl(` or a raw
`oklch(` **in a class string or a style value** is a bug. `color-mix()` against a
token is fine. The one place a hex is allowed is a comment *citing a capture* —
"`#2b3547` against `#151a23` in `settings.svg`" is provenance for a decision, and
it is worth more in the file than in a commit message.

## Conventions

- `React.ComponentProps<"div">` and spread the rest, so `title`, `data-*`,
  `aria-*` and handlers reach the DOM without every component listing them.
- `cn()` from `@/lib/utils` for class merging — it is `clsx` + `tailwind-merge`,
  so a caller's `className` genuinely overrides rather than losing to specificity.
- `data-slot="<name>"` on the root element, as shadcn's own components do.
- Export the component and, where it has variants, its `cva` too.
- A doc comment saying **why**, not what. For this pass the most useful thing
  that can be written above a component is *the run off the capture it draws*.
- **Painting order is load-bearing wherever a rule is cut.** A pseudo-element
  rule must be `::before`, never `::after`: both it and the label are
  positioned, so DOM order decides which is on top, and `::after` draws the rule
  straight through the words. This was found in a screenshot, not in review —
  when you change a frame, look at it.
