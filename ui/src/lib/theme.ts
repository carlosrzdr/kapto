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
 * Colours for series of data and kinds of event, in order: told apart from
 * one another with each kind of colour blindness, at least 3:1 on the dark
 * surfaces, and of none of the state colours' hues (green, sky blue, amber,
 * red), which keep their meaning. The same as the `--series-<n>` tokens of
 * `app.css`, for what CSS colours. Few, as those hues take the rest: past
 * them a series is told apart by its line's pattern, never by a colour
 * used again in the same way.
 */
export const SERIES = ["#2f6bf5", "#8a7d14", "#bc10ba", "#9d7dc9"];

/**
 * The patterns a series' line is drawn in, as uPlot's canvas dashes take
 * them (pixels drawn, then left out), solid first: the `dash-<n>` swatches
 * of a plot's key draw the same.
 */
export const DASHES: number[][] = [[], [6, 4], [2, 3], [10, 3, 2, 3]];

/** How a series is drawn: its colour and its line's pattern, each with its place in its list. */
export interface SeriesStyle {
  color: string;
  /** Its place in [`SERIES`], the `series-<n>` class. */
  hue: number;
  dash: number[];
  /** Its place in [`DASHES`], the `dash-<n>` class. */
  pattern: number;
}

/**
 * How the series at `place` of a plot is drawn: every colour solid first,
 * then each again dashed, then dotted, then dash-dotted, so no two series
 * of a plot look the same until [`SERIES`] times [`DASHES`] of them, more
 * than one plot can show apart.
 */
export function seriesStyle(place: number): SeriesStyle {
  const hue = place % SERIES.length;
  const pattern = Math.floor(place / SERIES.length) % DASHES.length;
  return { color: SERIES[hue]!, hue, dash: DASHES[pattern]!, pattern };
}
