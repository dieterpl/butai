# Writing a page

**Read this, then `src/components/KIT.md`, before writing a page.**

A page is a **pure component**. It takes the slice of the world it draws and a
bag of callbacks, and it renders. It does not fetch, it does not own a socket, it
does not decide which workspace is current, and it does not talk to the daemon.

That split is not tidiness. `src/app/world.ts` owns the event streams and the
push/poll fallback; `src/app/actions.ts` owns everything that writes. A page that
fetched would be a third place the daemon is reached from, and the first one
nobody remembers to look at when a call goes wrong.

## The props every page takes

```ts
export interface PageProps {
  world: World;                 // src/app/world.ts — every daemon, every workspace
  ws: QualifiedWorkspace | null;// the current workspace, already qualified
  actions: Actions;             // src/app/actions.ts — everything that writes
  focus: string;                // which rail/panel the keyboard is on
  on: PageCallbacks;            // view-state changes: select, walk, open, focus
}
```

`actions` throws nothing: every method reports through a toast and returns. A
page never needs a `try`.

## Ids are qualified. All of them.

A pane id is `"gpu:5"`, not `5`. Two daemons both have a pane 5, and a bare
integer attaches to whichever answered first — which looks like a working
terminal on the wrong machine. `logic/events.ts` has `qid`, `daemonOf` and
`localId`; use them and never split the string by hand.

**A bare number compared against a qualified id never matches**, which is the
property that makes a forgotten qualification render nothing instead of rendering
someone else's machine. Do not "fix" that by loosening a comparison.

## Compose from the kit

One `Row`, one `SectionTitle`, one `Button`. If a page needs a shape
`src/components/` lacks, it is added there with its reason — never inlined. See
`KIT.md`; the scale is fixed and `p-1.5` is the smell.

Mono is semantic: paths, diffs, shas, the stage, numbers in a column. Everything
else is sans.

## What the old page is, and is not, evidence of

`web/ui/<name>.js` is the same page written for the previous kit. **Its logic and
its doc comments are the reference** — which rail shows what, what `enter` does
on each row, why a verb is where it is. Its *geometry* is not: it was written for
24px rows, mono everywhere, no elevation and no motion, and this pass moved
deliberately away from all four.

Where the old page worked around a missing component (its own truncation, its own
header), use the kit's instead.

## Do not

- Do not edit `src/app/*`, `src/components/*`, `src/logic/*`, `src/stage/*`, or
  another agent's page.
- Do not `git add`, `git commit`, or `git checkout` — several sessions share this
  working tree.
- Do not add a dependency.

## Verify

```sh
cd web && bun x tsc --noEmit 2>&1 | grep '<yourpage>'   # must print nothing
```

Then `src/pages/HANDOVER-<name>.md`: what you ported, what the old page did that
you could not carry, and anything you found wrong (report, do not fix).
