import { describe, expect, it } from "vitest";

import { boxesOf, directionOf, framed, leftAt, placedOf, sameBoxes } from "./views";

const close = (actual: number[], expected: number[]) => actual.forEach((value, axis) => expect(value).toBeCloseTo(expected[axis]!, 6));

describe("a robot panel's viewpoint", () => {
  it("frames the robot from in front, its right, above, or the front right and above by default", () => {
    const center: [number, number, number] = [0, 0, 0.5];
    close(framed("front", center, 2).from, [2, 0, 0.5]);
    close(framed("side", center, 2).from, [0, -2, 0.5]);
    // From above, a touch behind it, so its front is up the panel.
    const above = framed("top", center, 2).from;
    expect(above[2]).toBeCloseTo(2.5, 3);
    expect(above[0]).toBeLessThan(0);
    expect(Math.abs(above[0])).toBeLessThan(0.05);
    // By default, as the view always was framed.
    const [x, y, z] = framed(null, center, 2).from;
    expect([x > 0, y < 0, z > 0.5]).toEqual([true, true, true]);
    expect(x).toBeCloseTo(-y, 9);
    expect(Math.hypot(x, y, z - 0.5)).toBeCloseTo(2, 9);
    // Always looking at the centre of what it frames.
    for (const view of ["front", "side", "top", null] as const) {
      expect(framed(view, center, 2).at).toEqual(center);
    }
  });

  it("frames a place left while arranging along its own direction, and starts there", () => {
    const view = { from: [0, 3, 1], at: [0, 1, 1] };
    close(directionOf(view), [0, 1, 0]);
    close(framed(view, [0, 0, 0.5], 1.5).from, [0, 1.5, 0.5]);
    expect(placedOf(view)).toEqual({ from: [0, 3, 1], at: [0, 1, 1] });
    expect(placedOf("front")).toBeNull();
    expect(placedOf(null)).toBeNull();
    // A place with nowhere to look along is framed as by default.
    expect(directionOf({ from: [1, 1, 1], at: [1, 1, 1] })).toEqual(directionOf(null));
  });

  it("is kept to the millimetre where it was left", () => {
    expect(leftAt([0.1 + 0.2, -1.23456, 2], [-0.0004, 0, 0.40049])).toEqual({ from: [0.3, -1.235, 2], at: [0, 0, 0.4] });
    // Never a negative zero, which a file would keep as -0.0.
    expect(Object.is(leftAt([-0.0001, 0, 0], [0, 0, 0]).from[0], 0)).toBe(true);
  });
});

describe("a robot panel's place on the canvas", () => {
  // A canvas over the whole view, 1000 × 600, 100 pixels down the page.
  const canvas = { left: 0, top: 100, right: 1000, bottom: 700 };

  it("is the panel's rectangle counted from the canvas's bottom left", () => {
    const panel = { left: 10, top: 110, right: 410, bottom: 410 };
    expect(boxesOf(panel, [], canvas)).toEqual({
      viewport: { x: 10, y: 290, width: 400, height: 300 },
      scissor: { x: 10, y: 290, width: 400, height: 300 },
    });
  });

  it("draws only the part its column shows when it scrolls, the camera's picture still filling the whole panel", () => {
    const column = { left: 0, top: 100, right: 500, bottom: 400 };
    const panel = { left: 10, top: 300, right: 410, bottom: 600 };
    expect(boxesOf(panel, [column], canvas)).toEqual({
      viewport: { x: 10, y: 100, width: 400, height: 300 },
      scissor: { x: 10, y: 300, width: 400, height: 100 },
    });
  });

  it("is nowhere while none of it shows: scrolled away, off the canvas, or without a size", () => {
    const column = { left: 0, top: 100, right: 500, bottom: 400 };
    expect(boxesOf({ left: 10, top: 450, right: 410, bottom: 700 }, [column], canvas)).toBeNull();
    expect(boxesOf({ left: 1100, top: 110, right: 1300, bottom: 300 }, [], canvas)).toBeNull();
    expect(boxesOf({ left: 0, top: 0, right: 0, bottom: 0 }, [], canvas)).toBeNull();
  });

  it("tells a place that moved from one that didn't", () => {
    const one = boxesOf({ left: 10, top: 110, right: 410, bottom: 410 }, [], canvas);
    expect(sameBoxes(one, boxesOf({ left: 10, top: 110, right: 410, bottom: 410 }, [], canvas))).toBe(true);
    expect(sameBoxes(one, boxesOf({ left: 10, top: 111, right: 410, bottom: 410 }, [], canvas))).toBe(false);
    expect(sameBoxes(one, null)).toBe(false);
    expect(sameBoxes(null, null)).toBe(true);
  });
});
