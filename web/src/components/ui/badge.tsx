import * as React from "react"

import { cn } from "../../lib/utils"

export function Badge({ className, ...props }: React.ComponentProps<"span">) {
  return (
    <span
      className={cn("inline-flex rounded-full bg-zinc-100 px-2.5 py-1 text-xs font-medium text-zinc-700", className)}
      {...props}
    />
  )
}
