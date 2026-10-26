import { describe, expect, it } from "vitest";

import { type Box, type ColumnBoxes, dragged, dropPlace, landing, shareTicks, sharesSaid, SMALLEST_PIXELS } from "./dragging";

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

describe("the shares an edge being dragged gives", () => {
  it("puts a tick at each end of each share either side of it, each one's shares an equal part of it", () => {
    // 4 shares over 0–400, the edge's room, then 8 over 410–810.
    expect(shareTicks([0, 400], [410, 810], [4, 8], [0, 810])).toEqual([0, 100, 200, 300, 400, 410, 460, 510, 560, 610, 660, 710, 760, 810]);
    expect(shareTicks([0, 300], [300, 600], [1, 1], [0, 600])).toEqual([0, 300, 300, 600]);
  });

  it("shows only the ticks within the part of a column that scrolls shown", () => {
    // Panels of a share each 200 pixels tall, the column shown from 250 to 700.
    expect(shareTicks([0, 400], [400, 1000], [2, 3], [250, 700])).toEqual([400, 400, 600]);
  });

  it("says the shares either side of the edge, the one before first", () => {
    expect(sharesSaid([4, 8], 0)).toBe("4 : 8");
    expect(sharesSaid([3, 1, 12], 1)).toBe("1 : 12");
    expect(sharesSaid([3], 0)).toBe("3 : ?");
  });
});

describe("where a panel carried would land", () => {
  const size = { width: 200, height: 250 };

  it("in a column, as long as it is from the panel it would go before, across the column's width", () => {
    expect(landing(columns, { column: 1, at: 1 }, size)).toEqual(box(410, 305, 800, 555));
    // At the end of a full column, kept within it.
    expect(landing(columns, { column: 1, at: 2 }, size)).toEqual(box(410, 350, 800, 600));
    // Longer than the column: the column's length.
    expect(landing(columns, { column: 0, at: 0 }, { width: 200, height: 900 })).toEqual(box(0, 0, 400, 600));
  });

  it("as a new column, its own width over the gap it goes in, at most an even share of the room, the columns' whole length", () => {
    expect(landing(columns, { between: 1 }, size)).toEqual(box(305, 0, 505, 600));
    expect(landing(columns, { between: 0 }, size)).toEqual(box(0, 0, 200, 600));
    expect(landing(columns, { between: 2 }, size)).toEqual(box(600, 0, 800, 600));
    expect(landing([], { between: 0 }, size)).toBeNull();
  });

  it("on a narrow page, the same turned", () => {
    const stacked: ColumnBoxes[] = [
      { box: box(0, 0, 800, 300), panels: [box(0, 0, 395, 300), box(405, 0, 800, 300)] },
      { box: box(0, 310, 800, 600), panels: [box(0, 310, 800, 600)] },
    ];
    expect(landing(stacked, { column: 0, at: 1 }, size, true)).toEqual(box(405, 0, 605, 300));
    expect(landing(stacked, { between: 1 }, size, true)).toEqual(box(0, 205, 800, 405));
  });
});
