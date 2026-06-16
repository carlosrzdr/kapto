// The page's colours where CSS can't reach them: the 3D view and the plots
// are drawn by script, so they read the same tokens `app.css` defines.

/** The token `--name` as the page has it, or `fallback` where it has none (e.g. in tests). */
export function token(name: string, fallback: string): string {
  if (typeof document === "undefined") {
    return fallback;
  }
  return getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim() || fallback;
}

/**
 * Colours for series of data and kinds of event, in order: distinct from
 * one another and readable on the dark surfaces, none of them the state
 * colours, which keep their meaning. The same as the `--series-<n>` tokens
 * of `app.css`, for what CSS colours.
 */
export const SERIES = ["#38bdf8", "#fbbf24", "#a78bfa", "#2dd4bf", "#f472b6", "#a3e635", "#fb923c", "#e879f9"];
