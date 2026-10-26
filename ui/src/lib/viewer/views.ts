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
 * Where a panel's camera goes to look at `center` from `distance` away (as
 * far as its fields of view need), from the panel's direction.
 */
export function framed(view: PanelView | null | undefined, center: Point, distance: number): Placed {
  const direction = directionOf(view);
  return {
    from: [0, 1, 2].map((axis) => center[axis]! + direction[axis]! * distance) as Point,
    at: [...center],
  };
}

/**
 * A box along the robot's axes, by its lowest and highest corner, and, if
 * known, the radius of a sphere around its middle the robot is within too.
 */
export interface Bounds {
  low: Point;
  high: Point;
  radius?: number;
}

/** The middle of `bounds`, where a camera framing it looks. */
export function middleOf(bounds: Bounds): Point {
  return [0, 1, 2].map((axis) => (bounds.low[axis]! + bounds.high[axis]!) / 2) as Point;
}

/**
 * How much of a panel's width or height, whichever binds, a framed box
 * takes: a little short of the edge, so the robot never touches it.
 */
export const FILLED = 0.95;

/**
 * How far from the middle of `bounds` a camera looking at it from
 * `direction` (from the point looked at to the camera) must be for the box
 * to fit a panel of `aspect` (width over height) with a vertical field of
 * view of `fovDegrees`: each corner within both fields of view, the side
 * that binds filled to `FILLED`. So a tall panel shows a tall robot by its
 * height, and a wide one a wide robot by its width. With a sphere around
 * it too, whichever of the two lets the camera come closer: the robot is
 * within both, and a box's far-out corners, near the camera, can need more
 * room than the sphere does.
 */
export function fittingDistance(direction: Point, bounds: Bounds, fovDegrees: number, aspect: number): number {
  const vertical = Math.tan(((fovDegrees * Math.PI) / 180) / 2) * FILLED;
  const horizontal = vertical * aspect;
  // The camera's own axes, Z up as the scene's: right across the panel and up it.
  const forward = direction.map((value) => -value) as Point;
  let right = cross(forward, [0, 0, 1]);
  if (Math.hypot(...right) < 1e-9) {
    right = [0, -1, 0];
  }
  right = unit(right);
  const up = cross(right, forward);
  const middle = middleOf(bounds);
  let distance = 0;
  for (let corner = 0; corner < 8; corner += 1) {
    const offset = [0, 1, 2].map((axis) => ((corner >> axis) & 1 ? bounds.high[axis]! : bounds.low[axis]!) - middle[axis]!) as Point;
    // How far towards the camera the corner is: nearer, it looks bigger.
    const toward = dot(offset, direction);
    distance = Math.max(distance, toward + Math.abs(dot(offset, right)) / horizontal, toward + Math.abs(dot(offset, up)) / vertical);
  }
  if (bounds.radius !== undefined && bounds.radius > 0) {
    // The sphere just inside the narrower field of view, as filled.
    distance = Math.min(distance, bounds.radius / Math.sin(Math.atan(Math.min(vertical, horizontal))));
  }
  // A box with no size: as close as a box a centimetre across.
  return Math.max(distance, 0.01 / vertical);
}

const dot = (a: Point, b: Point) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a: Point, b: Point): Point => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const unit = (a: Point): Point => {
  const length = Math.hypot(...a) || 1;
  return [a[0] / length, a[1] / length, a[2] / length];
};

/**
 * Where a panel's camera goes to fill its panel with `bounds`, seen from
 * `view`'s side: a preset's or the default's direction, or for a place, the
 * way it looks.
 */
export function fitted(view: PanelView | null | undefined, bounds: Bounds, fovDegrees: number, aspect: number): Placed {
  return framed(view, middleOf(bounds), fittingDistance(directionOf(view), bounds, fovDegrees, aspect));
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
