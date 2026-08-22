// Path — **the directory shrinks, the basename never does.**
//
// The audit's worst finding: the CHANGES rail's eight rows all read
// `crates/butai-client/src/…`, which is eight rows carrying one fact. The part
// that told them apart was exactly the part `text-overflow: ellipsis` cut,
// because a single truncating box always eats the end.
//
// So a path is two boxes — a directory that may shrink to nothing and a
// basename that may not shrink at all — and the ellipsis therefore lands in the
// *middle* of the string: `crates/butai-cli…mod.rs`. A rail too narrow for both
// loses the directory, which is the half you can infer.
//
// Pure CSS, deliberately: no measurement, no `ResizeObserver`, no re-render on
// resize, and it is correct at every width including the ones between two
// frames. See `HANDOVER-data.md` for the one case this does not solve.
//
// `font-mono` because a path is a thing you compare character by character —
// two paths that differ in one character have to be diffable by eye, and a
// proportional font hides exactly that difference.

import * as React from "react";

import { cn } from "@/lib/utils";

type PathProps = Omit<React.ComponentProps<"span">, "children"> & {
  /** The path, as the daemon spells it — `crates/butai-client/src/chrome/mod.rs`. */
  path: string;
};

function Path({ className, path, ...props }: PathProps) {
  const cut = path.lastIndexOf("/") + 1;
  const dir = path.slice(0, cut);
  const name = path.slice(cut);
  return (
    <span
      data-slot="path"
      // The whole path, for the row that is too narrow for it. Before the
      // spread, so a caller with something better to say can still say it.
      title={path}
      {...props}
      className={cn("flex min-w-0 flex-1 items-baseline overflow-hidden font-mono", className)}
    >
      {dir ? <span className="truncate text-faint">{dir}</span> : null}
      {/* No colour of its own: the basename takes the row's, so a caller that
          dims the whole path dims this too and the directory stays a step
          fainter than it. Bare filenames truncate — there is nothing else in
          the box to give up. */}
      <span className={dir ? "shrink-0" : "truncate"}>{name}</span>
    </span>
  );
}

export { Path };
