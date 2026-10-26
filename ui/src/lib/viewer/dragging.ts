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

/** Where along the page something starts and ends, in pixels. */
export type Span = [start: number, end: number];

/**
 * Where the ticks go along the two either side of an edge being dragged:
 * one at each end of each of their shares, each split into as many equal
 * parts as it has shares, so a share reads as a step. Only those within
 * `within`, the part shown of a column that scrolls.
 */
export function shareTicks(before: Span, after: Span, shares: [number, number], within: Span): number[] {
  const ends = ([start, end]: Span, count: number) => {
    const parts = Math.max(count, 1);
    return Array.from({ length: parts + 1 }, (_, step) => start + (step * (end - start)) / parts);
  };
  return [...ends(before, shares[0]), ...ends(after, shares[1])].filter((at) => at >= within[0] - 0.5 && at <= within[1] + 0.5);
}

/** The shares either side of the edge after `edge`, as they are said beside it: "4 : 8". */
export function sharesSaid(shares: number[], edge: number): string {
  return `${shares[edge] ?? "?"} : ${shares[edge + 1] ?? "?"}`;
}

/**
 * Where a panel `size` big, dropped at `to`, would show: in a column, its
 * own length along it from where it would go, kept within the part of the
 * column shown; as a new column, its own width there, at most an even
 * share of the room with the others, so it reads as narrower than a
 * column it would go into, the columns' whole length. Turned when
 * `stacked`, as [`dropPlace`] is.
 */
export function landing(columns: ColumnBoxes[], to: Place, size: { width: number; height: number }, stacked = false): Box | null {
  // Along the columns (a), and along the panels of one (b).
  const a = (box: Box): Span => (stacked ? [box.top, box.bottom] : [box.left, box.right]);
  const b = (box: Box): Span => (stacked ? [box.left, box.right] : [box.top, box.bottom]);
  const boxOf = ([a0, a1]: Span, [b0, b1]: Span): Box =>
    stacked ? { left: b0, right: b1, top: a0, bottom: a1 } : { left: a0, right: a1, top: b0, bottom: b1 };
  const across = stacked ? size.height : size.width;
  const along = stacked ? size.width : size.height;

  if ("column" in to) {
    const column = columns[to.column];
    if (!column) {
      return null;
    }
    const [c0, c1] = b(column.box);
    const length = Math.min(along, c1 - c0);
    const next = column.panels[to.at];
    const last = column.panels[column.panels.length - 1];
    const from = next ? b(next)[0] : last ? b(last)[1] : c0;
    const start = Math.max(c0, Math.min(from, c1 - length));
    return boxOf(a(column.box), [start, start + length]);
  }
  const first = columns[0];
  const last = columns[columns.length - 1];
  if (!first || !last) {
    return null;
  }
  const [g0, g1] = [a(first.box)[0], a(last.box)[1]];
  const before = columns[to.between - 1];
  const after = columns[to.between];
  const at = before && after ? (a(before.box)[1] + a(after.box)[0]) / 2 : after ? a(after.box)[0] : g1;
  const width = Math.min(across, (g1 - g0) / (columns.length + 1));
  const start = Math.max(g0, Math.min(at - width / 2, g1 - width));
  const ends = columns.map((each) => b(each.box));
  return boxOf([start, start + width], [Math.min(...ends.map(([from]) => from)), Math.max(...ends.map(([, to]) => to))]);
}
