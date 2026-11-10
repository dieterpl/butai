// The HELP page: butai's own reference, as a page you enter, read and leave.
//
// **It was the DOCS page, and that is what this fixes.** 0.9 made the reference
// a folder in the DOCS rail and opened a topic in the file viewer, so pressing
// help rebuilt the *file* screen around a listing that was not files — with a
// breadcrumb, a `..` row, a find button and an editor that had to refuse to
// save. 0.10 took it out of that space entirely. `chrome/help.rs` carries the
// argument: a press on help is not a request to browse a project.
//
// The browser client still nests the reference inside DOCS. This is that lag
// closed, on the terms SETTINGS already set:
//
//   * **a page of its own**, not a view of one workspace — nothing here is
//     about the project you have open, and no daemon is in the loop at all,
//     which is why it reads the same over ssh as it does locally;
//   * **a contents column down the left and the topic beside it**, across the
//     whole band rather than squeezed between two rails, because a reference in
//     a narrow column is the modal problem again in a different frame;
//   * **`esc` back to whatever you were doing** rather than a place in the page
//     rotation.
//
// ## The topics are generated, and that is the point
//
// Every topic comes from `logic/docs.ts`'s `topics()`, which lays out
// `verbs.ts`'s `reference()` as markdown — the same generator the `?` reference
// has always used, so a surface cannot fall out of the reference while its keys
// keep working. **Nothing here is hand-written.** Two references that agree
// today are two references; one generator rendered in one place cannot disagree
// with itself, and hand-writing a topic in this file would be the second one.
//
// `readMarkdown` returns blocks rather than markup and `Prose` builds elements
// from text nodes, so there is no `innerHTML` anywhere on this path.
//
// ## Keys: the one topic that is a drawing
//
// `Keys` is the same tables laid on a keyboard rather than in a table —
// `logic/keyboard.ts` reads `GLOBAL` and `allSurfaces()` exactly as
// `reference()` does, and `HelpKeys.tsx` draws the result. It is a row in the
// contents like any other, placed after the section it is the picture of; it is
// not one of `topics()` because that list is also the DOCS rail's, and a
// keyboard is not a markdown file. Its slug is `keyboard`, because `keys` is
// already where `?` lands.
//
// ## The one place this leaves the terminal's key table
//
// `help.rs`'s footer offers `tab next page`. This page does not bind `tab`: in
// a browser it is how you reach the contents list at all, and taking it would
// buy a topic-cycler at the cost of the keyboard route to the list that already
// cycles topics. The other three — `j/k`, `home/end`, `esc` — are here and do
// what they do in the terminal.

import * as React from "react";

import { HintBar, type HintLike } from "@/components/HintBar";
import { Prose } from "@/components/Prose";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { HELP_TOPIC, readMarkdown, slugFor, topicFor, topics, type Topic } from "@/logic/docs.ts";
import { ARROW_ALIASES, VerbId, helpVerbs, keyName, reference } from "@/logic/verbs.ts";
import { KeysTopic } from "./HelpKeys.tsx";

// The contents column. `help.rs` gives it 24 columns for a longest row of 19;
// this is the same column in pixels, sized to the widest title the generator
// produces (`The pointer's alone`) at the row scale.
const LIST_W = "w-56";

// One press of `j`. `Prose` sets its body at `text-13 leading-relaxed`, which
// is where this number comes from — a scroll step that is not a line of the
// text reads as a jump rather than as reading.
const LINE = 21;

/** A topic, plus the title `reference()` gave the section it was built from. */
interface Entry {
  topic: Topic;
  title: string;
  /** Drawn as the keyboard rather than as prose. */
  keyboard?: boolean;
}

/** The Keys topic's slug — not `keys`, which is `HELP_TOPIC`'s. */
export const KEYBOARD_TOPIC = "keyboard";

/** The Keys topic: a row in the contents with no markdown behind it. */
const KEYBOARD: Entry = {
  topic: { slug: KEYBOARD_TOPIC, name: KEYBOARD_TOPIC, path: "butai://" + KEYBOARD_TOPIC, title: "Keys", body: "" },
  title: "Keys",
  keyboard: true,
};

/**
 * The contents, in the generator's order.
 *
 * The title is looked up through `slugFor` rather than by position: `topics()`
 * maps `reference()` one for one today, but two arrays that happen to be
 * parallel are a bug waiting for someone to filter one of them.
 */
function useEntries(prefix: string | undefined): Entry[] {
  return React.useMemo(() => {
    const titles = new Map(reference().map((sec) => [slugFor(sec.title), sec.title]));
    const out: Entry[] = topics(prefix).map((topic) => ({ topic, title: titles.get(topic.slug) ?? topic.slug }));
    // After the two layers, which it draws; at the end if that section is gone.
    const at = out.findIndex((e) => `butai://${e.topic.slug}` === HELP_TOPIC);
    out.splice(at < 0 ? out.length : at + 1, 0, KEYBOARD);
    return out;
  }, [prefix]);
}

export interface HelpPageProps {
  /**
   * The prefix key as the user spells it — `settings.ts`'s
   * `readPrefixSpelling`. The reference writes `C-b` and `topics()` rewrites it,
   * because a reference that names a key you did not configure is the one page
   * that must not be wrong.
   */
  prefix?: string | undefined;
  /**
   * Which topic is open, by slug. Given, this page is controlled and the shell
   * owns the position — which is what lets you walk away mid-page and come back
   * to where you stopped, as the terminal's does. Absent, the page keeps it.
   */
  topic?: string | undefined;
  onTopic?: ((slug: string) => void) | undefined;
  /**
   * `esc`, and the close button. Absent leaves the hint as documentation rather
   * than as a control that does nothing.
   */
  onClose?: (() => void) | undefined;
}

export function HelpPage({ prefix, topic, onTopic, onClose }: HelpPageProps) {
  const entries = useEntries(prefix);
  // Where `?` lands: the two key layers, which is what the modal this descends
  // from held and what `?` means to anyone who has used tmux. Resolved through
  // `topicFor` so a retitled section cannot silently move it.
  const first = topicFor(HELP_TOPIC, prefix)?.slug ?? entries[0]?.topic.slug ?? "";
  const [own, setOwn] = React.useState<string>(first);
  const slug = topic ?? own;
  const current = entries.find((e) => e.topic.slug === slug) ?? entries[0] ?? null;

  const root = React.useRef<HTMLDivElement>(null);
  const body = React.useRef<HTMLDivElement>(null);

  const blocks = React.useMemo(
    () => (current?.keyboard ? [] : readMarkdown(current?.topic.body)),
    [current],
  );

  // Entered deliberately — by `?`, or by a menu — so it takes the keyboard on
  // arrival rather than waiting for a click that a keyboard user has no way to
  // make. `preventScroll` because focusing an element scrolls it into view, and
  // this one fills the band already.
  React.useEffect(() => {
    root.current?.focus({ preventScroll: true });
  }, []);

  // The scroll is per page rather than per topic, for `help.rs`'s reason:
  // arriving halfway down something you have not read yet is never what was
  // meant, and a scroll position per topic is eleven numbers to keep true.
  React.useEffect(() => {
    body.current?.scrollTo({ top: 0 });
  }, [slug]);

  const choose = (next: string) => {
    setOwn(next);
    onTopic?.(next);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
    // The Keys topic's search box: `j` is a letter there, not a scroll.
    if (isField(e.target)) return;
    const name = keyName(e.nativeEvent);
    const alias = ARROW_ALIASES[name];
    const verbs = helpVerbs();
    const id = verbs.find((v) => (alias ? v.id === alias : v.key === name))?.id;
    if (id === VerbId.CloseHelp) {
      if (!onClose) return;
      e.preventDefault();
      onClose();
      return;
    }
    const el = body.current;
    if (!el) return;
    switch (id) {
      case VerbId.Down:
        el.scrollBy({ top: LINE });
        break;
      case VerbId.Up:
        el.scrollBy({ top: -LINE });
        break;
      case VerbId.PageDown:
        el.scrollBy({ top: el.clientHeight - LINE });
        break;
      case VerbId.PageUp:
        el.scrollBy({ top: -(el.clientHeight - LINE) });
        break;
      case VerbId.Top:
        el.scrollTo({ top: 0 });
        break;
      case VerbId.Bottom:
        el.scrollTo({ top: el.scrollHeight });
        break;
      default:
        // Every other key is the shell's. A page that swallowed them would take
        // the global layer down with it for as long as help was open.
        return;
    }
    e.preventDefault();
  };

  return (
    <div
      data-page="help"
      ref={root}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      className="flex h-full min-h-0 flex-col bg-background outline-none"
    >
      <div className="flex min-h-0 flex-1">
        <aside className={cn("flex shrink-0 flex-col border-r border-border bg-card", LIST_W)}>
          <SectionTitle>contents</SectionTitle>
          <div role="listbox" aria-label="contents" className="min-h-0 flex-1 overflow-y-auto">
            {entries.map((e) => (
              <Row
                key={e.topic.slug}
                selected={e.topic.slug === current?.topic.slug}
                onSelect={() => choose(e.topic.slug)}
              >
                <span className="min-w-0 truncate">{e.title}</span>
              </Row>
            ))}
          </div>
        </aside>

        <section className="flex min-w-0 flex-1 flex-col">
          <SectionTitle
            action={
              onClose ? (
                <Button variant="ghost" size="sm" onClick={onClose}>
                  close
                </Button>
              ) : null
            }
          >
            help
          </SectionTitle>
          {/* `Prose` draws the topic's own `# ` heading, so there is no title
              here to say the same thing twice. */}
          <div ref={body} className="min-h-0 flex-1 overflow-y-auto">
            {current?.keyboard ? <KeysTopic prefix={prefix} /> : <Prose blocks={blocks} />}
          </div>
        </section>
      </div>

      {/* Spanning the page, because these keys are the page's and not the
          list's — which is exactly what `HintBar`'s position claims. */}
      <HintBar
        keys={[
          ...(current?.keyboard ? KEYBOARD_HINTS : []),
          ["j/k", "scroll"],
          ["home/end", "top · bottom"],
          { key: "esc", label: "close", onSelect: onClose },
        ]}
      />
    </div>
  );
}

/** What the Keys topic adds to the page's own keys. */
const KEYBOARD_HINTS: readonly HintLike[] = [
  ["/", "find"],
  ["alt", "hold for the alt layer"],
];

function isField(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el?.tagName) return false;
  const tag = el.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || el.isContentEditable;
}

export default HelpPage;
