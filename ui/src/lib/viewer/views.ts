// The robot seen from several sides at once: one scene, one canvas over the
// whole view, and each robot panel a camera onto it, drawn into that
// panel's place on the canvas.

import type { PanelView } from "../api-types";

/** A point or a direction, x y z, in metres in the robot's frame. */
export type Point = [number, number, number];

/** A rectangle on the page, as `getBoundingClientRect` gives it. */
export interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** A rectangle on the canvas as three.js takes it: from its bottom left, in CSS pixels. */
export interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Where a camera is and what it looks at. */
export interface Placed {
  from: Point;
  at: Point;
}

/**
 * Which way a panel's camera looks at the robot, as the direction from the
 * point looked at to the camera: from the front right and a little above
 * by default; along the robot's x axis from in front, along y from its
 * right, along z from above (a touch behind, so its front is up the
 * panel); a place's own direction.
 */
export function directionOf(view: PanelView | null | undefined): Point {
  const along = (x: number, y: number, z: number): Point => {
    const length = Math.hypot(x, y, z) || 1;
    return [x / length, y / length, z / length];
  };
  if (view === "front") {
    return along(1, 0, 0);
  }
  if (view === "side") {
    return along(0, -1, 0);
  }
  if (view === "top") {
    return along(-0.01, 0, 1);
  }
  if (view && typeof view === "object") {
    const [x = 0, y = 0, z = 0] = view.from.map((value, axis) => value - (view.at[axis] ?? 0));
    if (Math.hypot(x, y, z) > 0) {
      return along(x, y, z);
    }
  }
  return along(1, -1, 0.7);
}

/**
 * Where a panel's camera goes to frame a sphere around `center` `distance`
 * away (as far as its fields of view need): looking at its centre, from
 * the panel's direction.
 */
export function framed(view: PanelView | null | undefined, center: Point, distance: number): Placed {
  const direction = directionOf(view);
  return {
    from: [0, 1, 2].map((axis) => center[axis]! + direction[axis]! * distance) as Point,
    at: [...center],
  };
}

/** Where a place in a layout puts the camera, before anything is framed; `null` for a preset or none. */
export function placedOf(view: PanelView | null | undefined): Placed | null {
  if (!view || typeof view !== "object") {
    return null;
  }
  const point = (values: number[]): Point => [values[0] ?? 0, values[1] ?? 0, values[2] ?? 0];
  return { from: point(view.from), at: point(view.at) };
}

/**
 * Where a camera was left, as a layout keeps it: to the millimetre, which
 * no one sees and which keeps the file short.
 */
export function leftAt(from: Point, at: Point): { from: Point; at: Point } {
  const round = (point: Point): Point => point.map((value) => Math.round(value * 1000) / 1000 + 0) as Point;
  return { from: round(from), at: round(at) };
}

/**
 * Where on the canvas a panel shows the robot: `viewport`, the panel's
 * whole place, which its camera's picture fills, and `scissor`, the part
 * of it that shows, inside every one of `clips` (the column it scrolls in)
 * and the canvas. `null` when none of it shows. Both from the canvas's
 * bottom left, as three.js takes them.
 */
export function boxesOf(panel: Rect, clips: Rect[], canvas: Rect): { viewport: Box; scissor: Box } | null {
  const height = canvas.bottom - canvas.top;
  const box = (rect: Rect): Box => ({
    x: rect.left - canvas.left,
    y: height - (rect.bottom - canvas.top),
    width: rect.right - rect.left,
    height: rect.bottom - rect.top,
  });
  const shown = [...clips, canvas].reduce<Rect>(
    (inside, clip) => ({
      left: Math.max(inside.left, clip.left),
      top: Math.max(inside.top, clip.top),
      right: Math.min(inside.right, clip.right),
      bottom: Math.min(inside.bottom, clip.bottom),
    }),
    panel,
  );
  if (shown.right - shown.left < 1 || shown.bottom - shown.top < 1) {
    return null;
  }
  return { viewport: box(panel), scissor: box(shown) };
}

/** Whether two panels' places are the same, so nothing needs drawing again for them. */
export function sameBoxes(one: { viewport: Box; scissor: Box } | null, other: { viewport: Box; scissor: Box } | null): boolean {
  if (one === null || other === null) {
    return one === other;
  }
  const same = (a: Box, b: Box) => a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
  return same(one.viewport, other.viewport) && same(one.scissor, other.scissor);
}
