import type { PageName } from "./Shell";
import { GLOBAL } from "../logic/verbs";

// Keep route ids stable; the words and order come from the terminal's Page::ORDER.
export const VIEWS: readonly PageName[] = ["work", "files", "git", "docker", "docs"];
export function pageLabel(page: PageName): string {
  return page === "work" ? "agents" : page === "home" ? "booth" : page;
}
export function viewShortcut(page: PageName, prefix: string): string {
  const verb = GLOBAL.find(v => v.label === pageLabel(page));
  if (verb?.alt) return `Alt+${verb.alt.toUpperCase()}`;
  return verb?.prefix ? `${prefix} ${verb.prefix}` : "";
}
export function cycleView(page: PageName, delta: number): PageName {
  const index = Math.max(0, VIEWS.indexOf(page));
  return VIEWS[(index + delta + VIEWS.length) % VIEWS.length]!;
}
