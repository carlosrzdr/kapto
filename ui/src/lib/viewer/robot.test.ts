import * as THREE from "three";
import { describe, expect, it, vi } from "vitest";

import type { RobotScene } from "../api-types";
import { applyPoses, buildRobot, viewingDistance } from "./robot";

const still = { translation: [0, 0, 0], rotation: [0, 0, 0, 1] };

const scene: RobotScene = {
  name: "cell",
  root: "base",
  meshes: 2,
  links: [
    { name: "base", visuals: [{ origin: still, color: [1, 0, 0, 1], shape: { kind: "box", size: [0.1, 0.2, 0.3] } }] },
    {
      name: "arm",
      visuals: [
        { origin: { translation: [0, 0, 0.5], rotation: [0, 0, 0, 1] }, color: [1, 1, 1, 1], shape: { kind: "cylinder", radius: 0.05, length: 0.4 } },
        { origin: still, color: [1, 1, 1, 1], shape: { kind: "mesh", mesh: 0, scale: [0.001, 0.001, 0.001] } },
        { origin: still, color: [1, 1, 1, 1], shape: { kind: "mesh", mesh: 1, scale: [1, 1, 1] } },
      ],
    },
  ],
};

const triangle = {
  positions: new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]),
  normals: null,
  colors: null,
  indices: new Uint32Array([0, 1, 2]),
  color: [0, 0, 1, 1] as [number, number, number, number],
};

describe("buildRobot", () => {
  it("makes a group per link with its shapes where the scene puts them, leaving out a mesh that can't be read", async () => {
    const missing = vi.fn();
    const mesh = vi.fn((n: number) => (n === 0 ? Promise.resolve([triangle]) : Promise.reject(new Error("gone"))));
    const robot = await buildRobot(scene, mesh, missing);
    expect(robot.links.map((link) => link.name)).toEqual(["base", "arm"]);
    const [cylinder, meshed, gone] = robot.links[1]!.children;
    expect(cylinder!.position.z).toBeCloseTo(0.5);
    // Along Z, as robot descriptions have it.
    const bounds = new THREE.Box3().setFromObject(cylinder!);
    expect(bounds.max.z - bounds.min.z).toBeCloseTo(0.4);
    expect(meshed!.scale.x).toBeCloseTo(0.001);
    expect(meshed!.children).toHaveLength(1);
    expect(gone!.children).toHaveLength(0);
    expect(missing).toHaveBeenCalledWith(1, expect.any(Error));
  });

  it("reads a mesh used twice once", async () => {
    const twice: RobotScene = {
      ...scene,
      links: [{ name: "base", visuals: [0, 1].map(() => ({ origin: still, color: [1, 1, 1, 1], shape: { kind: "mesh" as const, mesh: 0, scale: [1, 1, 1] } })) }],
    };
    const mesh = vi.fn(() => Promise.resolve([triangle]));
    await buildRobot(twice, mesh);
    expect(mesh).toHaveBeenCalledTimes(1);
  });
});

describe("applyPoses", () => {
  it("places each link from seven numbers, and refuses poses for another robot", () => {
    const links = [new THREE.Group(), new THREE.Group()];
    const turned = Math.SQRT1_2;
    expect(applyPoses(links, new Float32Array([0, 0, 0, 0, 0, 0, 1, 0.1, 0.2, 0.3, 0, 0, turned, turned]))).toBe(true);
    expect(links[1]!.position.toArray().map((value) => Number(value.toFixed(3)))).toEqual([0.1, 0.2, 0.3]);
    expect(links[1]!.quaternion.z).toBeCloseTo(turned);
    expect(applyPoses(links, new Float32Array(7))).toBe(false);
  });
});

describe("viewingDistance", () => {
  it("fits a sphere by the vertical field of view in a wide view and the horizontal one in a tall view", () => {
    const wide = viewingDistance(1, 45, 2);
    expect(wide).toBeCloseTo(1 / Math.sin(THREE.MathUtils.degToRad(22.5)));
    const tall = viewingDistance(1, 45, 0.5);
    const horizontal = 2 * Math.atan(Math.tan(THREE.MathUtils.degToRad(22.5)) * 0.5);
    expect(tall).toBeCloseTo(1 / Math.sin(horizontal / 2));
    expect(tall).toBeGreaterThan(wide);
  });
});

describe("a robot with nothing to draw", () => {
  it("gets a dot at each link so it can still be seen moving", async () => {
    const bare: RobotScene = { name: "bare", root: "a", meshes: 1, links: [
      { name: "a", visuals: [] },
      { name: "b", visuals: [{ origin: still, color: [1, 1, 1, 1], shape: { kind: "mesh", mesh: 0, scale: [1, 1, 1] } }] },
    ] };
    const robot = await buildRobot(bare, () => Promise.reject(new Error("gone")));
    for (const link of robot.links) {
      expect(link.children.some((child) => child instanceof THREE.Mesh)).toBe(true);
    }
    // One that draws something gets none.
    const drawn = await buildRobot(scene, () => Promise.resolve([triangle]));
    expect(drawn.links[0]!.children.some((child) => child instanceof THREE.Mesh)).toBe(false);
  });
});
