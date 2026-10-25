// Arranging the view by dragging on it: where a panel dropped at a point
// goes, and the sizes an edge dragged gives. Only the geometry, worked out
// from the boxes the page measured, so it is tested without a page.

import type { Place } from "./layouts";

/** A box on the page, as `getBoundingClientRect` gives it. */
export interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** A column's box, and its panels' boxes in order. */
export interface ColumnBoxes {
  box: Box;
  panels: Box[];
}

/** How near a column's side a drop starts a new column beside it, at most. */
const SIDE_PIXELS = 48;

/**
 * Where a panel dropped at (`x`, `y`) goes. Columns run across the page,
 * or one under another when `stacked` (a narrow page), their panels then
 * side by side: the same rules, turned. Near a column's side, or past the
 * columns, it goes into a new column there; anywhere else in a column,
 * before the first of its panels whose middle is past the point.
 */
export function dropPlace(columns: ColumnBoxes[], x: number, y: number, stacked = false): Place | null {
  if (columns.length === 0) {
    return null;
  }
  // Along the columns, and along the panels of one.
  const across = stacked ? y : x;
  const along = stacked ? x : y;
  const start = (box: Box) => (stacked ? box.top : box.left);
  const end = (box: Box) => (stacked ? box.bottom : box.right);
  const first = (box: Box) => (stacked ? box.left : box.top);
  const last = (box: Box) => (stacked ? box.right : box.bottom);

  const column = columns.findIndex((each) => across < end(each.box));
  if (column < 0) {
    return { between: columns.length };
  }
  const { box, panels } = columns[column]!;
  if (across < start(box)) {
    // In the gap before it.
    return { between: column };
  }
  const side = Math.min(SIDE_PIXELS, (end(box) - start(box)) / 5);
  if (across < start(box) + side) {
    return { between: column };
  }
  if (across > end(box) - side) {
    return { between: column + 1 };
  }
  const at = panels.findIndex((panel) => along < (first(panel) + last(panel)) / 2);
  return { column, at: at < 0 ? panels.length : at };
}

/** The smallest a column or panel is dragged to, in pixels. */
export const SMALLEST_PIXELS = 48;

/**
 * `sizes` with the edge after `edge` dragged `by` pixels: the one before
 * it grows by as much as the one after shrinks, neither past
 * [`SMALLEST_PIXELS`]; the rest stay.
 */
export function dragged(sizes: number[], edge: number, by: number): number[] {
  const before = sizes[edge];
  const after = sizes[edge + 1];
  if (before === undefined || after === undefined) {
    return sizes;
  }
  const moved = Math.max(SMALLEST_PIXELS - before, Math.min(after - SMALLEST_PIXELS, by));
  const next = [...sizes];
  next[edge] = before + moved;
  next[edge + 1] = after - moved;
  return next;
}
