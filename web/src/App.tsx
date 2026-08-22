// The kit gallery: every component on one page, in whichever palette you pick.
//
// This is not decoration. `UI-REWRITE.md`'s audit was only possible because
// somebody laid the old client's components side by side and *counted* — four
// section headers, three selection styles, six button shapes. A gallery is the
// page that makes that countable, so drift is visible before it ships rather
// than after somebody notices two pages disagreeing.
//
// It is also the only page that exists before the app does, which makes it the
// first thing that can be looked at in a browser.

import { useState } from "react";
import { Toaster } from "@/components/ui/sonner";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { Input } from "@/components/ui/input";

import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { HintBar } from "@/components/HintBar";
import { Empty } from "@/components/Empty";
import { Notice } from "@/components/Notice";
import { Stat } from "@/components/Stat";
import { Meter } from "@/components/Meter";
import { Gauge } from "@/components/Gauge";
import { Path } from "@/components/Path";
import { DiffStat } from "@/components/DiffStat";
import { Patch } from "@/components/Patch";

import { storeTheme, storedTheme, useTheme } from "./theme.ts";
import { themeNames } from "./logic/settings.ts";

const PATCH = `@@ -20,7 +20,7 @@ export const PROTO_VERSION = 1;
 // Cargo.toml. check.py asserts the two are equal whenever the Rust source is
 // alongside — a constant that drifts makes the mismatch banner below lie.
-export const CLIENT_VERSION = "0.9.0";
+export const CLIENT_VERSION = "0.10.0";

 // The oldest daemon we will send \`watch\` to.`;

// Eight paths that share a prefix — the audit's worst finding, and the one a
// gallery has to carry real data for. Truncated at the end they all read
// `crates/butai-client/src/…`, which is every row saying nothing.
const PATHS = [
  "crates/butai-client/src/chrome/mod.rs",
  "crates/butai-client/src/chrome/usage.rs",
  "crates/butai-client/src/workbench.rs",
  "crates/butai-server/src/pane/terminal.rs",
  "web/src/protocol/generated/protocol.ts",
];

export function App() {
  const [theme, setTheme] = useState(storedTheme);
  const pal = useTheme(theme);
  const [sel, setSel] = useState(1);
  const [tab, setTab] = useState("work");

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-full min-h-0 flex-col bg-background text-foreground">
        <header className="flex h-row-lg shrink-0 items-center gap-3 border-b border-border bg-card px-4 shadow-xs">
          <span className="text-16 font-semibold tracking-tight">butai</span>
          <Badge variant="secondary">kit</Badge>
          <Tabs value={tab} onValueChange={setTab}>
            <TabsList>
              {["work", "home", "git"].map((t) => (
                <TabsTrigger key={t} value={t}>
                  {t}
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
          <div className="flex-1" />
          <span className="text-11 text-faint">{pal ? pal.label : ""}</span>
          <select
            aria-label="theme"
            className="h-row rounded-md border border-border bg-card px-2 text-12 text-foreground"
            value={theme}
            onChange={(e) => setTheme(storeTheme(e.target.value))}
          >
            {themeNames().map((n: string) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </header>

        <main className="min-h-0 flex-1 overflow-auto p-4">
          <div className="mx-auto grid max-w-6xl grid-cols-1 items-start gap-4 lg:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle>Buttons</CardTitle>
              </CardHeader>
              <CardContent className="flex flex-col gap-3">
                <div className="flex flex-wrap items-center gap-2">
                  {(["default", "secondary", "outline", "ghost", "destructive"] as const).map((v) => (
                    <Button key={v} variant={v} onClick={() => toast(`${v} pressed`)}>
                      {v}
                    </Button>
                  ))}
                </div>
                <div className="flex flex-wrap items-center gap-2">
                  <Button size="sm">small</Button>
                  <Button size="default">default</Button>
                  <Button size="lg">large</Button>
                  <Button size="icon" aria-label="add">
                    +
                  </Button>
                  <Button disabled>disabled</Button>
                </div>
                <p className="text-12 text-dim">
                  `[+ agent]`, as the TUI writes it. The variant picks a pen, not a fill — except the default,
                  which is the accent band the terminal paints behind the thing you are looking at.
                </p>
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Badges, tooltips, input</CardTitle>
              </CardHeader>
              <CardContent className="flex flex-col gap-3">
                <div className="flex flex-wrap items-center gap-2">
                  {(["default", "secondary", "outline", "destructive"] as const).map((v) => (
                    <Badge key={v} variant={v}>
                      {v}
                    </Badge>
                  ))}
                </div>
                <div className="flex items-center gap-2">
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Button variant="outline" size="sm">
                        hover me
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent>A tooltip, from Radix, themed by the same tokens</TooltipContent>
                  </Tooltip>
                  <Button variant="outline" size="sm" onClick={() => toast.success("staged 3 files")}>
                    toast
                  </Button>
                </div>
                <Input placeholder="commit message…" />
              </CardContent>
            </Card>

            <Card className="lg:col-span-2">
              <SectionTitle action={<Button size="sm" variant="ghost">+ agent</Button>}>agents</SectionTitle>
              <div className="flex flex-col">
                {[
                  { title: "claude", state: "running", path: PATHS[0]! },
                  { title: "codex", state: "finished", path: PATHS[1]! },
                  { title: "aider", state: "waiting", path: PATHS[2]! },
                ].map((a, i) => (
                  <Row key={a.title} selected={sel === i} onSelect={() => setSel(i)}>
                    <span className="w-24 shrink-0 font-medium">{a.title}</span>
                    <Badge variant={a.state === "waiting" ? "destructive" : "outline"}>{a.state}</Badge>
                    <Path className="min-w-0 flex-1 text-dim" path={a.path} />
                  </Row>
                ))}
              </div>
              <HintBar
                keys={[
                  ["enter", "open"],
                  ["x", "kill"],
                  ["n", "new"],
                  ["?", "keys"],
                ]}
              />
            </Card>

            <Card>
              <SectionTitle>changes</SectionTitle>
              <div className="flex flex-col">
                {PATHS.map((p, i) => (
                  <Row key={p} compact>
                    <Path className="min-w-0 flex-1" path={p} />
                    <DiffStat added={[102, 3, 0, 41, 7][i]!} deleted={[0, 3, 12, 1, 7][i]!} />
                  </Row>
                ))}
              </div>
              <p className="px-3 py-2 text-11 text-faint">
                Elided in the middle, so the filename survives — and the counts form a column.
              </p>
            </Card>

            <Card>
              <SectionTitle>system</SectionTitle>
              <CardContent className="flex flex-col gap-3 pt-3">
                <Gauge label="cpu" value={62} max={100} suffix="%" />
                <Gauge label="ram" value={11.4} max={32} suffix=" GB" tone="warn" />
                <Gauge label="disk" value={97} max={100} suffix="%" tone="bad" />
                <Separator />
                <Stat label="agents" value="4" />
                <Stat label="processes" value="3" />
                <Stat label="uptime" value="38d" />
                <Meter value={30} max={100} />
              </CardContent>
            </Card>

            <Card className="lg:col-span-2">
              <SectionTitle>diff</SectionTitle>
              <Patch text={PATCH} />
            </Card>

            <Card>
              <SectionTitle>states</SectionTitle>
              <CardContent className="flex flex-col gap-3 pt-3">
                <Notice variant="warn">The daemon is 0.9.0 and this client is 0.10.0 — restart it.</Notice>
                <Notice variant="bad">rebase stopped on a conflict in 2 files</Notice>
                <Notice>4 commits behind origin/main</Notice>
                <Empty>not a git repository</Empty>
              </CardContent>
            </Card>
          </div>
        </main>
        <Toaster />
      </div>
    </TooltipProvider>
  );
}
