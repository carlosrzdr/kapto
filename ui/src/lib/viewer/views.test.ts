import * as THREE from "three";
import { describe, expect, it } from "vitest";

import {
  type Bounds,
  boxesOf,
  directionOf,
  eased,
  EASING_MS,
  FILLED,
  fitsView,
  fitted,
  fittingDistance,
  framed,
  leftAt,
  placedOf,
  type Point,
  sameBoxes,
  widenedTo,
} from "./views";

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

describe("a robot framed to fill its panel", () => {
  /** How much of the panel's width and height `bounds` takes, seen as three.js draws it from `view`'s fitted place. */
  const shown = (view: Parameters<typeof fitted>[0], bounds: Bounds, aspect: number) => {
    const camera = new THREE.PerspectiveCamera(45, aspect, 0.01, 100);
    camera.up.set(0, 0, 1);
    const { from, at } = fitted(view, bounds, camera.fov, aspect);
    camera.position.fromArray(from);
    camera.lookAt(...at);
    camera.updateMatrixWorld();
    const corners = [...Array(8).keys()].map((corner) =>
      new THREE.Vector3(...[0, 1, 2].map((axis) => ((corner >> axis) & 1 ? bounds.high[axis]! : bounds.low[axis]!))).project(camera),
    );
    const span = (axis: "x" | "y") => {
      const values = corners.map((corner) => corner[axis]);
      // Within the panel, from -1 to 1 across it.
      expect(Math.min(...values)).toBeGreaterThanOrEqual(-1);
      expect(Math.max(...values)).toBeLessThanOrEqual(1);
      return (Math.max(...values) - Math.min(...values)) / 2;
    };
    return { width: span("x"), height: span("y") };
  };
  // An arm's reach: about as wide as deep, a little taller, standing on the floor.
  const arm: Bounds = { low: [-0.6, -0.6, 0], high: [0.6, 0.6, 1.4] };
  const views = ["front", "side", "top", null] as const;

  it("fits inside a wide, square or tall panel from each side, filling it along the side that binds", () => {
    for (const aspect of [2.5, 1, 0.4]) {
      for (const view of views) {
        const { width, height } = shown(view, arm, aspect);
        expect(Math.max(width, height), `${view} at ${aspect}`).toBeGreaterThan(FILLED - 0.15);
        expect(Math.max(width, height), `${view} at ${aspect}`).toBeLessThanOrEqual(FILLED + 1e-9);
      }
    }
  });

  it("fills a tall panel by its height and a wide one by its width when the robot is that shape", () => {
    const standing: Bounds = { low: [-0.2, -0.2, 0], high: [0.2, 0.2, 1.4] };
    expect(shown("front", standing, 0.4).height).toBeGreaterThan(FILLED - 0.15);
    expect(shown("side", standing, 0.4).height).toBeGreaterThan(FILLED - 0.15);
    const lying: Bounds = { low: [-0.3, -1.2, 0], high: [0.3, 1.2, 0.5] };
    expect(shown("front", lying, 2.5).width).toBeGreaterThan(FILLED - 0.15);
    // Seen from above, a tall robot is no taller than it is wide: one side fills.
    const { width, height } = shown("top", arm, 0.4);
    expect(Math.max(width, height)).toBeGreaterThan(FILLED - 0.15);
  });

  it("is nearer than the sphere around the box would put it, so the robot looks bigger", () => {
    for (const aspect of [2.5, 1, 0.4]) {
      const { from, at } = fitted("front", arm, 45, aspect);
      const half = THREE.MathUtils.degToRad(22.5);
      const narrower = Math.min(2 * half, 2 * Math.atan(Math.tan(half) * aspect));
      const sphere = Math.hypot(0.6, 0.6, 0.7) / Math.sin(narrower / 2);
      expect(new THREE.Vector3(...from).distanceTo(new THREE.Vector3(...at))).toBeLessThan(sphere);
    }
  });

  it("looks at the middle of the box, along a place's own direction", () => {
    const place = { from: [0, 3, 1], at: [0, 1, 1] };
    const { from, at } = fitted(place, arm, 45, 1);
    close(at, [0, 0, 0.7]);
    expect(from[0]).toBeCloseTo(0, 9);
    expect(from[2]).toBeCloseTo(0.7, 9);
    expect(from[1]).toBeGreaterThan(0.6);
    expect(Math.max(...Object.values(shown(place, arm, 1)))).toBeGreaterThan(FILLED - 0.15);
  });

  it("keeps a box with no size in view, close by", () => {
    const point: Point = [0.1, 0.2, 0.3];
    const { from, at } = fitted("side", { low: point, high: point }, 45, 1);
    close(at, point);
    expect(Number.isFinite(from[1]) && from[1] < point[1]).toBe(true);
  });
});

describe("a robot kept in view as it moves", () => {
  // An arm at rest, standing narrow; then reaching out to its right.
  const resting: Bounds = { low: [-0.1, -0.15, 0], high: [0.45, 0.15, 0.9] };
  const reaching: Bounds = { low: [-0.1, -0.7, 0], high: [0.45, 0.15, 0.9] };
  const reach: Bounds = { low: [-1, -1, -0.2], high: [1, 1, 1.4] };
  const aspect = 0.5;
  /** A view framed on the arm at rest, from in front: where it looks from and at, and how far. */
  const start = () => {
    const { from, at } = fitted("front", resting, 45, aspect);
    const distance = Math.hypot(...from.map((value, axis) => value - at[axis]!));
    return { direction: directionOf("front"), at, distance };
  };
  /** The furthest any corner of `bounds` is from the middle of a view `distance` from `at`, as three.js draws it: 1 is its edge. */
  const furthest = (at: Point, distance: number, bounds: Bounds) => {
    const camera = new THREE.PerspectiveCamera(45, aspect, 0.01, 100);
    camera.up.set(0, 0, 1);
    camera.position.set(at[0] + distance, at[1], at[2]);
    camera.lookAt(...at);
    camera.updateMatrixWorld();
    const corners = [...Array(8).keys()].map((corner) =>
      new THREE.Vector3(...[0, 1, 2].map((axis) => ((corner >> axis) & 1 ? bounds.high[axis]! : bounds.low[axis]!))).project(camera),
    );
    return Math.max(...corners.map((corner) => Math.max(Math.abs(corner.x), Math.abs(corner.y))));
  };

  it("fits the view it was framed in, and doesn't once it reaches past its margin", () => {
    const { direction, at, distance } = start();
    expect(fitsView(direction, at, distance, resting, 45, aspect)).toBe(true);
    expect(furthest(at, distance, resting)).toBeCloseTo(FILLED, 6);
    expect(fitsView(direction, at, distance, reaching, 45, aspect)).toBe(false);
    expect(furthest(at, distance, reaching)).toBeGreaterThan(FILLED);
  });

  it("widens about the same point just far enough to fit it again with its margin", () => {
    const { direction, at, distance } = start();
    const widened = widenedTo(direction, at, distance, reaching, 45, aspect, reach);
    expect(widened).toBeGreaterThan(distance);
    expect(widened).toBeCloseTo(fittingDistance(direction, reaching, 45, aspect, at), 9);
    expect(fitsView(direction, at, widened, reaching, 45, aspect)).toBe(true);
    expect(fitsView(direction, at, widened * 0.99, reaching, 45, aspect)).toBe(false);
    // Each corner within the panel, the furthest at the margin.
    expect(furthest(at, widened, reaching)).toBeCloseTo(FILLED, 6);
  });

  it("never comes nearer by itself, however small the robot stands", () => {
    const { direction, at, distance } = start();
    const far = widenedTo(direction, at, distance, reaching, 45, aspect, reach);
    expect(widenedTo(direction, at, far, resting, 45, aspect, reach)).toBe(far);
    expect(widenedTo(direction, at, distance, resting, 45, aspect, reach)).toBe(distance);
  });

  it("never widens beyond fitting everywhere the robot reaches, and without a reach as far as it takes", () => {
    const { direction, at, distance } = start();
    // A pose far out of reach, as a stream gone wrong could send.
    const astray: Bounds = { low: [0, 40, 0], high: [0.2, 41, 0.2] };
    const outermost = fittingDistance(direction, reach, 45, aspect, at);
    expect(widenedTo(direction, at, distance, astray, 45, aspect, reach)).toBeCloseTo(outermost, 9);
    expect(widenedTo(direction, at, distance, astray, 45, aspect, null)).toBeGreaterThan(outermost * 10);
  });

  it("eases out over a few hundred milliseconds, never past where it goes", () => {
    let distance = 1;
    const goal = 2;
    const path: number[] = [];
    for (let ms = 0; ms < 3 * EASING_MS; ms += 16) {
      distance = eased(distance, goal, 16);
      path.push(distance);
    }
    // Moving out each frame, most of the way there in three time constants.
    expect(path.every((each, at) => each > (path[at - 1] ?? 1) && each <= goal)).toBe(true);
    expect(distance).toBeGreaterThan(1.9);
    expect(eased(1, goal, 0)).toBe(1);
    // Within a millimetre, there.
    expect(eased(1.9995, goal, 16)).toBe(goal);
    expect(eased(1, goal, 10 * EASING_MS)).toBe(goal);
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
