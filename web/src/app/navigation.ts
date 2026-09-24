import type { PageName } from "./Shell";

// Keep route ids stable; the words and order come from the terminal's Page::ORDER.
export const VIEWS: readonly PageName[] = ["work", "files", "git", "docker", "docs", "usage"];
export function pageLabel(page: PageName): string {
  return page === "work" ? "agents" : page === "home" ? "booth" : page;
}
export function cycleView(page: PageName, delta: number): PageName {
  const index = Math.max(0, VIEWS.indexOf(page));
  return VIEWS[(index + delta + VIEWS.length) % VIEWS.length]!;
}
