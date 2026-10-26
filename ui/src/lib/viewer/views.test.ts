import * as THREE from "three";
import { describe, expect, it } from "vitest";

import { type Bounds, boxesOf, directionOf, FILLED, fitted, framed, leftAt, placedOf, type Point, sameBoxes } from "./views";

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
        expect(Math.max(width, height), `${view} at ${aspect}`).toBeGreaterThan(0.8);
        expect(Math.max(width, height), `${view} at ${aspect}`).toBeLessThanOrEqual(FILLED + 1e-9);
      }
    }
  });

  it("fills a tall panel by its height and a wide one by its width when the robot is that shape", () => {
    const standing: Bounds = { low: [-0.2, -0.2, 0], high: [0.2, 0.2, 1.4] };
    expect(shown("front", standing, 0.4).height).toBeGreaterThan(0.8);
    expect(shown("side", standing, 0.4).height).toBeGreaterThan(0.8);
    const lying: Bounds = { low: [-0.3, -1.2, 0], high: [0.3, 1.2, 0.5] };
    expect(shown("front", lying, 2.5).width).toBeGreaterThan(0.8);
    // Seen from above, a tall robot is no taller than it is wide: one side fills.
    const { width, height } = shown("top", arm, 0.4);
    expect(Math.max(width, height)).toBeGreaterThan(0.8);
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

  it("comes as close as the sphere around the robot allows when its box's corners need more room", () => {
    const distance = (bounds: Bounds, aspect: number) => {
      const { from, at } = fitted("front", bounds, 45, aspect);
      return new THREE.Vector3(...from).distanceTo(new THREE.Vector3(...at));
    };
    // An arm turning all the way round: its reach a sphere, the box's corners out of it.
    const round: Bounds = { low: [-1, -1, -0.4], high: [1, 1, 1.4], radius: 1.1 };
    const boxOnly: Bounds = { low: round.low, high: round.high };
    for (const aspect of [2.5, 1, 0.4]) {
      expect(distance(round, aspect)).toBeLessThan(distance(boxOnly, aspect));
      // The sphere still inside the panel, its binding side filled.
      const half = Math.atan(Math.tan(THREE.MathUtils.degToRad(22.5)) * Math.min(1, aspect) * FILLED);
      expect(Math.asin(1.1 / distance(round, aspect))).toBeCloseTo(half, 9);
    }
    // A sphere looser than the box changes nothing.
    expect(distance({ ...arm, radius: 5 }, 0.4)).toBeCloseTo(distance(arm, 0.4), 9);
  });

  it("looks at the middle of the box, along a place's own direction", () => {
    const place = { from: [0, 3, 1], at: [0, 1, 1] };
    const { from, at } = fitted(place, arm, 45, 1);
    close(at, [0, 0, 0.7]);
    expect(from[0]).toBeCloseTo(0, 9);
    expect(from[2]).toBeCloseTo(0.7, 9);
    expect(from[1]).toBeGreaterThan(0.6);
    expect(Math.max(...Object.values(shown(place, arm, 1)))).toBeGreaterThan(0.8);
  });

  it("keeps a box with no size in view, close by", () => {
    const point: Point = [0.1, 0.2, 0.3];
    const { from, at } = fitted("side", { low: point, high: point }, 45, 1);
    close(at, point);
    expect(Number.isFinite(from[1]) && from[1] < point[1]).toBe(true);
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
