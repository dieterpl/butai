// Prose — rendered markdown: one measure, one rhythm.
//
// The last sans surface in the client, and it is sans no longer. The previous
// pass argued that "writing set in a monospace font at full bleed is the same
// paragraph made harder to read", which is true of a web page and beside the
// point here: `docs/images/help.svg` is the TUI drawing this exact document —
// the key reference, headings, paragraphs and tables — on the same 8.4 x 18
// cell as every row around it. A second family for the one page that reads as a
// document is how the client stopped looking like one thing.
//
// What survives is the *measure*: 78 characters, which is a count of cells
// rather than a pixel width, so it holds at any size. `help.svg` sets its body
// to about 72 columns beside a rail, so this is the same shape.
//
// ## Blocks, not markup, and it stays that way
//
// `docs.ts`'s `readMarkdown` returns **data** — `{kind, spans}` — never an HTML
// string, because a parser that returns markup is one `innerHTML` away from a
// README in somebody's repository being script on this page, and this client
// renders whatever the daemon hands it. This component is the other half of
// that property: it builds the elements itself, from text nodes, so there is no
// path from a file's contents to markup at all.
//
// That is why it takes `blocks` rather than `children`. A `<Prose>` you can put
// arbitrary nodes into is a `<Prose>` somebody eventually puts
// `dangerouslySetInnerHTML` into, and the guarantee is gone without anything
// looking wrong at the call site.

import * as React from "react";

import { cn } from "@/lib/utils";
import { Code } from "@/components/Code";
import type { Block, Span } from "@/logic/docs.ts";

// The measure. Not a token because it is not on any scale here — it is a count
// of characters, so it tracks the font size rather than a pixel width, which is
// what a measure has to do.
const PROSE = "min-w-0 max-w-[78ch] p-3 font-mono text-13 leading-[18px] text-foreground";

// One size, so the levels are told apart by weight, colour and a rule — which
// is all a terminal has to tell them apart with, and is what `help.svg` does:
// `Keys` and `The Alt layer` are the same cell as the paragraph under them.
const HEADING = {
  h1: "mt-4 mb-2 font-semibold text-primary first:mt-0",
  // A rule under the section head rather than caps in `--dim`: `SectionTitle`
  // owns the caps-and-dim voice in this kit, and a document borrowing it makes
  // its own headings read as chrome that wandered into the text.
  h2: "mt-4 mb-2 border-b border-border pb-1 font-semibold first:mt-0",
  h3: "mt-3 mb-1 font-semibold first:mt-0",
  h4: "mt-3 mb-1 font-semibold tracking-caps text-dim uppercase first:mt-0",
} as const;

const TAG = { 1: "h1", 2: "h2", 3: "h3", 4: "h4" } as const;

// No box around inline code any more: the page is already the code's family, so
// a tinted pill is the only thing that would say "this is different" — and in
// the TUI it is said with the pen instead.
const INLINE_CODE = "text-primary";
const LINK = "text-primary underline underline-offset-2 hover:text-foreground";

/** Inline spans: code, links, emphasis, and text. */
function Spans({ spans }: { spans: readonly Span[] }) {
  return (
    <>
      {spans.map((s, i) => {
        if (s.code) {
          return (
            <code key={i} className={INLINE_CODE}>
              {s.text}
            </code>
          );
        }
        // `target` and `rel` because a link in a project's README goes wherever
        // that project's author pointed it, and this page is not a place to
        // open it: a new tab keeps the workbench, and `noopener` keeps it ours.
        if (s.href) {
          return (
            <a key={i} href={s.href} target="_blank" rel="noopener noreferrer" className={LINK}>
              {s.text}
            </a>
          );
        }
        if (s.strong) {
          return (
            <strong key={i} className="font-semibold">
              {s.text}
            </strong>
          );
        }
        if (s.em) {
          return (
            <em key={i} className="italic">
              {s.text}
            </em>
          );
        }
        return <React.Fragment key={i}>{s.text}</React.Fragment>;
      })}
    </>
  );
}

function Blk({ block }: { block: Block }) {
  switch (block.kind) {
    case "h": {
      const level = Math.min(4, Math.max(1, block.level)) as 1 | 2 | 3 | 4;
      const Tag = TAG[level];
      return (
        <Tag className={HEADING[Tag]}>
          <Spans spans={block.spans} />
        </Tag>
      );
    }
    // A fenced block is the same box `Code` and `Patch` are, framed — one code
    // surface in the client, whether it arrived from a file or from a document
    // about a file. The language is carried by `docs.ts` and ignored here:
    // there is no highlighter, and a wrong one is worse than none.
    case "code":
      return <Code text={block.text} className="my-2 rounded-none border border-border bg-card" />;
    case "rule":
      return <hr className="my-4 border-0 border-t border-border" />;
    case "quote":
      return (
        <blockquote className="my-2 border-l border-border pl-2 text-dim">
          <Spans spans={block.spans} />
        </blockquote>
      );
    case "ul":
      return (
        <ul className="my-2 list-disc pl-4 marker:text-faint">
          {block.items.map((item, i) => (
            <li key={i} className="my-1">
              <Spans spans={item} />
            </li>
          ))}
        </ul>
      );
    // Its own scroller, for the same reason `Patch` has one: the reference's key
    // tables are the widest thing on this page, and a page that scrolls sideways
    // takes the rails with it.
    case "table":
      return (
        <div className="my-2 overflow-x-auto">
          <table className="w-full border-collapse">
            <tbody>
              {block.rows.map((row, y) => (
                <tr key={y}>
                  {row.map((cell, x) =>
                    // The first row is the header — `readMarkdown` drops the
                    // `|---|` separator, which is the thing that said so.
                    y === 0 ? (
                      <th
                        key={x}
                        scope="col"
                        className="py-0 pr-3 text-left align-top font-semibold tracking-caps text-dim uppercase"
                      >
                        <Spans spans={cell} />
                      </th>
                    ) : (
                      <td key={x} className="border-t border-border py-0 pr-3 align-top first:whitespace-nowrap">
                        <Spans spans={cell} />
                      </td>
                    ),
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    default:
      return (
        <p className="my-2">
          <Spans spans={block.spans} />
        </p>
      );
  }
}

type ProseProps = Omit<React.ComponentProps<"div">, "children"> & {
  /** A document, as `readMarkdown` read it. Data — see the note above. */
  blocks: readonly Block[];
};

function Prose({ className, blocks, ...props }: ProseProps) {
  return (
    <div data-slot="prose" {...props} className={cn(PROSE, className)}>
      {blocks.map((block, i) => (
        <Blk key={i} block={block} />
      ))}
    </div>
  );
}

export { Prose };
