import { describe, expect, test } from "bun:test";

import {
  LABEL_SUGGESTIONS,
  MAX_LABELS,
  cleanLabel,
  knownLabels,
  roleOf,
  tagsOf,
  toggleLabel,
  toggleRole,
} from "../src/logic/tags.ts";

describe("roles", () => {
  test("only the four known words are roles", () => {
    expect(roleOf({ role: "lead" })).toBe("lead");
    expect(roleOf({ role: "boss" })).toBeNull();
    expect(roleOf({ role: null })).toBeNull();
    expect(roleOf({})).toBeNull();
  });

  test("picking the current role clears it, and labels are left alone", () => {
    const a = { role: "worker", labels: ["bug"] };
    expect(toggleRole(a, "worker")).toEqual({ role: null, labels: ["bug"] });
    expect(toggleRole(a, "lead")).toEqual({ role: "lead", labels: ["bug"] });
  });

  test("a daemon that sends neither field reads as untagged", () => {
    expect(tagsOf({})).toEqual({ role: null, labels: [] });
  });
});

describe("labels", () => {
  test("cleanLabel mirrors the daemon's rule", () => {
    expect(cleanLabel("#Review")).toBe("review");
    expect(cleanLabel("  a_b-c ")).toBe("a_b-c");
    expect(cleanLabel("two words")).toBeNull();
    expect(cleanLabel("")).toBeNull();
    expect(cleanLabel("x".repeat(25))).toBeNull();
    expect(cleanLabel("x".repeat(24))).toBe("x".repeat(24));
  });

  test("toggling adds in order and removes what is there", () => {
    const a = { role: "lead", labels: ["bug"] };
    expect(toggleLabel(a, "idea")).toEqual({ role: "lead", labels: ["bug", "idea"] });
    expect(toggleLabel(a, "bug")).toEqual({ role: "lead", labels: [] });
  });

  test("a ninth label is refused rather than sent", () => {
    const full = { labels: Array.from({ length: MAX_LABELS }, (_, i) => `l${i}`) };
    expect(toggleLabel(full, "extra")).toBeNull();
    expect(toggleLabel(full, "l0")?.labels).toHaveLength(MAX_LABELS - 1);
  });

  test("the menu lists suggestions first, then labels in use elsewhere, once each", () => {
    const got = knownLabels([{ labels: ["bug", "infra"] }, { labels: ["infra", "perf"] }, {}]);
    expect(got.slice(0, LABEL_SUGGESTIONS.length)).toEqual([...LABEL_SUGGESTIONS]);
    expect(got.slice(LABEL_SUGGESTIONS.length)).toEqual(["infra", "perf"]);
  });
});
