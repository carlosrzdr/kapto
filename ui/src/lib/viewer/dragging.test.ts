import { describe, expect, it } from "vitest";

import { type Box, type ColumnBoxes, dragged, dropPlace, SMALLEST_PIXELS } from "./dragging";

const box = (left: number, top: number, right: number, bottom: number): Box => ({ left, top, right, bottom });

/** Two columns side by side, 0–400 and 410–800; the second's two panels one over the other. */
const columns: ColumnBoxes[] = [
  { box: box(0, 0, 400, 600), panels: [box(0, 0, 400, 600)] },
  { box: box(410, 0, 800, 600), panels: [box(410, 0, 800, 295), box(410, 305, 800, 600)] },
];

describe("dropping a dragged panel", () => {
  it("goes before the first panel whose middle is past the point", () => {
    expect(dropPlace(columns, 600, 100)).toEqual({ column: 1, at: 0 });
    expect(dropPlace(columns, 600, 200)).toEqual({ column: 1, at: 1 });
    expect(dropPlace(columns, 600, 590)).toEqual({ column: 1, at: 2 });
    expect(dropPlace(columns, 200, 400)).toEqual({ column: 0, at: 1 });
  });

  it("starts a new column near a column's side, in the gap between two, or past the last", () => {
    expect(dropPlace(columns, 10, 300)).toEqual({ between: 0 });
    expect(dropPlace(columns, 395, 300)).toEqual({ between: 1 });
    expect(dropPlace(columns, 405, 300)).toEqual({ between: 1 });
    expect(dropPlace(columns, 790, 300)).toEqual({ between: 2 });
    expect(dropPlace(columns, 900, 300)).toEqual({ between: 2 });
    expect(dropPlace([], 0, 0)).toBeNull();
  });

  it("on a narrow page, with the columns one under another, follows the same rules turned", () => {
    const stacked: ColumnBoxes[] = [
      { box: box(0, 0, 800, 300), panels: [box(0, 0, 395, 300), box(405, 0, 800, 300)] },
      { box: box(0, 310, 800, 600), panels: [box(0, 310, 800, 600)] },
    ];
    expect(dropPlace(stacked, 300, 150, true)).toEqual({ column: 0, at: 1 });
    expect(dropPlace(stacked, 100, 590, true)).toEqual({ between: 2 });
  });
});

describe("dragging an edge", () => {
  it("grows the one before it by what the one after shrinks, neither past the smallest", () => {
    expect(dragged([400, 400, 200], 0, 100)).toEqual([500, 300, 200]);
    expect(dragged([400, 400], 0, 1000)).toEqual([800 - SMALLEST_PIXELS, SMALLEST_PIXELS]);
    expect(dragged([400, 400], 0, -1000)).toEqual([SMALLEST_PIXELS, 800 - SMALLEST_PIXELS]);
    // The last has no edge after it.
    expect(dragged([400, 400], 1, 50)).toEqual([400, 400]);
  });
});
