// What a chat is for: a role, and labels.
//
// The port of `normalize_tags` in `crates/butai-protocol/src/api.rs`, plus the
// two gestures a menu makes. Pure — no DOM, no network — so `test/tags.test.ts`
// runs the lot under bun. The daemon is the last word on what is valid; this
// only stops the menu from offering what it would refuse.

/// The roles, in the order a menu lists them. Mirrors `AGENT_ROLES`.
export const ROLES = ["lead", "worker", "loop", "watch"] as const;
export type Role = (typeof ROLES)[number];

/// What a role means, said in the menu beside its name.
export const ROLE_HINT: Readonly<Record<Role, string>> = Object.freeze({
  lead: "orchestrates",
  worker: "takes tasks",
  loop: "runs on repeat",
  watch: "waits on events",
});

/// A role is a word in a pen, like every other state on this client — see
/// `components/ui/badge.tsx`. The four tones differ in hue *and* the words
/// differ, so nothing relies on colour alone.
export const ROLE_TONE: Readonly<Record<Role, string>> = Object.freeze({
  lead: "text-warn",
  worker: "text-primary",
  loop: "text-ok",
  watch: "text-info",
});

/// Labels offered before anyone has made one. Others in use in the fleet are
/// added to the menu as they appear.
export const LABEL_SUGGESTIONS: readonly string[] = Object.freeze(["review", "blocked", "bug", "idea"]);

export const MAX_LABELS = 8;
export const MAX_LABEL_LEN = 24;

/// What a chat carries. Both fields are optional on the wire: a daemon older
/// than the feature sends neither.
export interface Tagged {
  role?: string | null | undefined;
  labels?: readonly string[] | undefined;
}

export interface Tags {
  role: Role | null;
  labels: string[];
}

/// The role, if it is one this client knows. An unknown word from a newer
/// daemon is shown as nothing rather than guessed at.
export function roleOf(a: Tagged): Role | null {
  return (ROLES as readonly string[]).includes(a.role ?? "") ? (a.role as Role) : null;
}

export function labelsOf(a: Tagged): string[] {
  return [...(a.labels ?? [])];
}

export function tagsOf(a: Tagged): Tags {
  return { role: roleOf(a), labels: labelsOf(a) };
}

/// One label as the daemon will accept it, or `null` when it will not.
export function cleanLabel(raw: string): string | null {
  const l = raw.trim().replace(/^#+/, "").toLowerCase();
  return l && l.length <= MAX_LABEL_LEN && /^[a-z0-9_-]+$/.test(l) ? l : null;
}

/// Pick a role, or clear it by picking the one it already is.
export function toggleRole(a: Tagged, role: Role): Tags {
  const t = tagsOf(a);
  return { role: t.role === role ? null : role, labels: t.labels };
}

/// Add a label, or remove it if the chat already has it. `null` when adding
/// would break a limit, so the caller can say so instead of sending it.
export function toggleLabel(a: Tagged, label: string): Tags | null {
  const t = tagsOf(a);
  if (t.labels.includes(label)) return { role: t.role, labels: t.labels.filter((l) => l !== label) };
  if (t.labels.length >= MAX_LABELS) return null;
  return { role: t.role, labels: [...t.labels, label] };
}

/// The labels a menu lists: the suggestions, then any other label a chat in the
/// fleet already uses, so one made once stays one click away.
export function knownLabels(chats: readonly Tagged[]): string[] {
  const out = [...LABEL_SUGGESTIONS];
  for (const c of chats) for (const l of c.labels ?? []) if (!out.includes(l)) out.push(l);
  return out;
}
