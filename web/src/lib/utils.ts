import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * shadcn's `cn`. `clsx` joins, `tailwind-merge` resolves conflicts.
 *
 * The old kit had neither and said so: "a `class` you pass is appended, not
 * merged — so it cannot reliably beat a class the component already set." That
 * was a real constraint dressed as a rule, and it is gone. `twMerge` knows the
 * v4 scale from the generated CSS, so `<Button class="h-row-lg">` now overrides
 * the size rather than sitting behind it and losing.
 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
