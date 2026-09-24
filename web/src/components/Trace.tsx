import { useEffect, useRef, useState } from "react";
import { brailleTrace } from "@/logic/trace";
import { cn } from "@/lib/utils";

/** A daemon's recorded history on the same braille grid as the terminal. */
export function Trace({ history, silent = false, className, prefix = "" }: {
  history: readonly number[]; silent?: boolean; className?: string; prefix?: string;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [cells, setCells] = useState(0);
  useEffect(() => {
    const element = host.current;
    if (!element) return;
    const context = document.createElement("canvas").getContext("2d")!;
    const observer = new ResizeObserver(() => {
      context.font = getComputedStyle(element).font;
      setCells(Math.max(0, Math.floor(element.clientWidth / context.measureText("0").width) - prefix.length));
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [prefix]);
  return <div ref={host} className={cn("h-row overflow-hidden whitespace-pre bg-status-bg font-mono text-13", className)}
    aria-label={`${prefix || "history"}: ${history.slice(-1)[0] ?? 0}`}>
    {prefix}{brailleTrace(history, cells, silent)}
  </div>;
}
