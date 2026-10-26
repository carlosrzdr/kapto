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

/** A box along the robot's axes, by its lowest and highest corner. */
export interface Bounds {
  low: Point;
  high: Point;
}

/** The middle of `bounds`, where a camera framing it looks. */
export function middleOf(bounds: Bounds): Point {
  return [0, 1, 2].map((axis) => (bounds.low[axis]! + bounds.high[axis]!) / 2) as Point;
}

/**
 * How much of a panel's width or height, whichever binds, a framed box
 * takes: most of it, with room for the robot to move a little before its
 * view has to widen.
 */
export const FILLED = 0.8;

/**
 * How far from `at` (by default the middle of `bounds`) a camera looking at
 * it from `direction` (a unit vector from the point looked at to the
 * camera) must be for the box to fit a panel of `aspect` (width over
 * height) with a vertical field of view of `fovDegrees`: each corner within
 * both fields of view, the side that binds filled to `FILLED`. So a tall
 * panel shows a tall robot by its height, and a wide one a wide robot by
 * its width. In numbers alone, as it runs each frame the robot moves.
 */
export function fittingDistance(direction: Point, bounds: Bounds, fovDegrees: number, aspect: number, at?: Point): number {
  const vertical = Math.tan(((fovDegrees * Math.PI) / 180) / 2) * FILLED;
  const horizontal = vertical * aspect;
  const [dx, dy, dz] = direction;
  // The camera's own axes, Z up as the scene's: right across the panel
  // (forward, the direction reversed, across Z) and up it (right across forward).
  let rx = -dy;
  let ry = dx;
  const across = Math.hypot(rx, ry);
  if (across < 1e-9) {
    rx = 0;
    ry = -1;
  } else {
    rx /= across;
    ry /= across;
  }
  const ux = -ry * dz;
  const uy = rx * dz;
  const uz = ry * dx - rx * dy;
  const { low, high } = bounds;
  const [mx, my, mz] = at ?? [(low[0] + high[0]) / 2, (low[1] + high[1]) / 2, (low[2] + high[2]) / 2];
  let distance = 0;
  for (let corner = 0; corner < 8; corner += 1) {
    const ox = (corner & 1 ? high[0] : low[0]) - mx;
    const oy = (corner & 2 ? high[1] : low[1]) - my;
    const oz = (corner & 4 ? high[2] : low[2]) - mz;
    // How far towards the camera the corner is: nearer, it looks bigger.
    const toward = ox * dx + oy * dy + oz * dz;
    distance = Math.max(distance, toward + Math.abs(ox * rx + oy * ry) / horizontal, toward + Math.abs(ox * ux + oy * uy + oz * uz) / vertical);
  }
  // A box with no size: as close as a box a centimetre across.
  return Math.max(distance, 0.01 / vertical);
}

/**
 * Whether `bounds` is within the view of a camera `distance` from `at`
 * along `direction`, with the room `FILLED` leaves around it.
 */
export function fitsView(direction: Point, at: Point, distance: number, bounds: Bounds, fovDegrees: number, aspect: number): boolean {
  return fittingDistance(direction, bounds, fovDegrees, aspect, at) <= distance * (1 + 1e-9);
}

/**
 * How far from `at` along `direction` a camera now `distance` away goes to
 * keep `bounds`, the robot as it stands, in view: just far enough to fit it
 * again with room around it, never beyond fitting all of `limit`, where
 * the robot can reach, and never nearer than it is.
 */
export function widenedTo(
  direction: Point,
  at: Point,
  distance: number,
  bounds: Bounds,
  fovDegrees: number,
  aspect: number,
  limit: Bounds | null,
): number {
  if (fitsView(direction, at, distance, bounds, fovDegrees, aspect)) {
    return distance;
  }
  const needed = fittingDistance(direction, bounds, fovDegrees, aspect, at);
  const outermost = limit ? fittingDistance(direction, limit, fovDegrees, aspect, at) : Infinity;
  return Math.max(distance, Math.min(needed, outermost));
}

/** How quickly a view widens: most of the way in three of these, a few hundred milliseconds. */
export const EASING_MS = 100;

/**
 * Where a camera `distance` away is `ms` later on its way out to `goal`:
 * quickly at first, slowing as it comes, and there once within a millimetre.
 */
export function eased(distance: number, goal: number, ms: number): number {
  const next = goal - (goal - distance) * Math.exp(-Math.max(0, ms) / EASING_MS);
  return goal - next < 0.001 ? goal : next;
}

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
