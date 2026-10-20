// The right-click menu on a chat: its role, and its labels.
//
// Opens **at the pointer** — a menu about the row you clicked belongs next to
// it, not in a dialog in the middle of the page — and flips to stay on screen.
// Radix owns the positioning, the focus and the keys (arrows, Enter, Escape),
// which is the part of a menu that goes wrong when written by hand.
//
// It stays open while you pick: a chat usually wants a role *and* a label or
// two, and reopening it for each is the slow way to do a quick thing. Escape or
// a click elsewhere closes it.
//
// The anchor is a zero-size fixed element at the pointer, because Radix's
// `ContextMenu` only opens from its own `contextmenu` event and this one is also
// opened from a button, for a keyboard or a trackpad.

import { DropdownMenu } from "radix-ui";

import { cn } from "@/lib/utils";
import { ROLES, ROLE_HINT, ROLE_TONE, tagsOf, type Tagged, type Tags } from "@/logic/tags.ts";

export interface TagMenuProps {
  /// Where the pointer was, in viewport pixels.
  at: { x: number; y: number };
  /// The chat the menu is about, as it is *now* — the menu redraws as the
  /// daemon's answer lands, so a pick shows its tick.
  chat: Tagged & { title: string };
  /// Labels to offer: the suggestions plus any in use elsewhere.
  labels: readonly string[];
  onTag(next: Tags): void;
  onToggleLabel(label: string): void;
  onNewLabel(): void;
  onClose(): void;
}

const ITEM = cn(
  "flex h-7 cursor-default items-center gap-2 px-2 text-12 outline-none select-none",
  "data-[highlighted]:bg-sel",
);

export function TagMenu({ at, chat, labels, onTag, onToggleLabel, onNewLabel, onClose }: TagMenuProps) {
  const t = tagsOf(chat);
  // Labels a chat carries that are not in the offered list (a ninth-most-used
  // one) must still be shown, or they could be added and never removed.
  const shown = [...labels, ...t.labels.filter((l) => !labels.includes(l))];
  return (
    <DropdownMenu.Root open onOpenChange={(open) => (open ? undefined : onClose())} modal>
      <DropdownMenu.Trigger asChild>
        <span aria-hidden style={{ position: "fixed", left: at.x, top: at.y, width: 0, height: 0 }} />
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="start"
          side="bottom"
          sideOffset={2}
          collisionPadding={8}
          className="z-50 min-w-56 border border-border bg-popover p-1 font-mono text-popover-foreground"
          aria-label={`Tag ${chat.title}`}
        >
          <DropdownMenu.Label className="truncate px-2 py-1 text-11 text-dim">role · {chat.title}</DropdownMenu.Label>
          {ROLES.map((r) => (
            <DropdownMenu.CheckboxItem
              key={r}
              checked={t.role === r}
              onSelect={(e) => e.preventDefault()}
              onCheckedChange={() => onTag({ role: t.role === r ? null : r, labels: t.labels })}
              className={ITEM}
            >
              <span className="w-3 shrink-0 text-dim">{t.role === r ? "●" : "○"}</span>
              <span className={cn("w-14 shrink-0", ROLE_TONE[r])}>{r}</span>
              <span className="text-dim">{ROLE_HINT[r]}</span>
            </DropdownMenu.CheckboxItem>
          ))}
          <DropdownMenu.Separator className="my-1 h-px bg-border" />
          <DropdownMenu.Label className="px-2 py-1 text-11 text-dim">labels</DropdownMenu.Label>
          {shown.map((l) => (
            <DropdownMenu.CheckboxItem
              key={l}
              checked={t.labels.includes(l)}
              onSelect={(e) => e.preventDefault()}
              onCheckedChange={() => onToggleLabel(l)}
              className={ITEM}
            >
              <span className="w-3 shrink-0 text-dim">{t.labels.includes(l) ? "✓" : " "}</span>
              <span className="text-dim">#</span>
              <span className="truncate">{l}</span>
            </DropdownMenu.CheckboxItem>
          ))}
          <DropdownMenu.Separator className="my-1 h-px bg-border" />
          <DropdownMenu.Item className={ITEM} onSelect={onNewLabel}>
            <span className="w-3 shrink-0" />
            new label…
          </DropdownMenu.Item>
          <DropdownMenu.Item
            className={cn(ITEM, "text-dim")}
            disabled={t.role == null && t.labels.length === 0}
            onSelect={() => onTag({ role: null, labels: [] })}
          >
            <span className="w-3 shrink-0" />
            clear tags
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
