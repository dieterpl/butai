// Patch — a unified diff, coloured by where each line came from.
//
// In the kit because **more than one page renders one**: GIT shows a commit or
// a stash, FILES shows a file's own diff, and in the old client that meant two
// components carrying the same four rules between them.
//
// It owns its horizontal scroll. A diff is the widest thing this client draws —
// two versions of a line plus a marker column — and a page that scrolls
// sideways takes its rails and its header along with it, so the overflow stops
// here.

import * as React from "react";

import { cn } from "@/lib/utils";
import { patchFiles, syntaxRuns } from "@/logic/patch";
import { Card } from "@/components/ui/card";
import { SectionTitle } from "@/components/SectionTitle";
import { DiffStat } from "@/components/DiffStat";
import { CODE_BOX } from "@/components/Code";

// Which colour a line takes. **The order is load-bearing** and it is the
// terminal client's (`renderPatch`): `+++` is tested after `+`, so a file
// header reads as an addition. The two clients draw the same patch, and a `@@`
// coloured differently in one of them is visibly two products.
//
// The last arm is therefore only reachable for `diff ` and `index ` lines
// today. It stays written out because it is what makes the ordering above a
// decision rather than an accident.
function patchTone(line: string): string {
  if (line.startsWith("+")) return "text-ok";
  if (line.startsWith("-")) return "text-bad";
  if (line.startsWith("@@")) return "font-semibold text-primary";
  if (
    line.startsWith("diff ") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ")
  ) {
    return "text-dim";
  }
  return "";
}

type PatchProps = Omit<React.ComponentProps<"div">, "children"> & {
  /** Raw unified-diff text, as `git diff` prints it — [`DiffDto`]'s `patch`. */
  text: string;
};

function Patch({ className, text, ...props }: PatchProps) {
  const files = patchFiles(text);
  return <div data-slot="patch" {...props} className={cn("min-w-0 overflow-auto", className)}>
    {files.map((file, index) => <Card key={index} className="mb-1 min-w-max">
      <SectionTitle action={<DiffStat added={file.added} deleted={file.deleted} />}>{file.name}</SectionTitle>
      <pre className={cn(CODE_BOX, "p-1")}>
        {file.lines.map((line, i) => {
          const numbered = line.old != null || line.next != null;
          const changed = numbered && (line.old == null || line.next == null);
          return <React.Fragment key={i}>
            <span aria-hidden="true" className="inline-block w-[10ch] select-none text-faint">
              <span className="inline-block w-[4ch] text-right">{line.old ?? ""}</span>{" "}
              <span className="inline-block w-[4ch] text-right">{line.next ?? ""}</span>{" "}
            </span>
            <span className={cn(changed && (line.old == null ? "bg-ok/10" : "bg-bad/10"))}>
              {numbered ? <><span className={patchTone(line.text)}>{line.text[0]}</span>
                <span>{syntaxRuns(line.text.slice(1)).map((run, j) => <span key={j} className={run.tone || undefined}>{run.text}</span>)}</span>
              </> : <span className={patchTone(line.text) || undefined}>{line.text}</span>}
              {"\n"}
            </span>
          </React.Fragment>;
        })}
      </pre>
    </Card>)}
  </div>;
}

export { Patch, patchTone };
