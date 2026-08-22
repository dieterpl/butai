import * as React from "react"

import { cn } from "@/lib/utils"

function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(
        // `Input`'s box, taller. See that file for why the focus state is a
        // swapped rule rather than a halo.
        "flex field-sizing-content min-h-16 w-full rounded-none border border-input bg-transparent",
        "px-2 py-1 text-13 leading-[18px] outline-none placeholder:text-faint",
        "focus-visible:border-ring disabled:cursor-not-allowed disabled:opacity-50",
        "aria-invalid:border-destructive",
        className
      )}
      {...props}
    />
  )
}

export { Textarea }
