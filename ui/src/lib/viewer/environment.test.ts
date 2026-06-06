import * as THREE from "three";
import { describe, expect, it } from "vitest";

import { applyPlacement, build, formatOf, memoryStore, placedMesh, placedShape } from "./environment";

/** A binary STL holding one triangle, as CAD programs write them. */
function oneTriangle(): ArrayBuffer {
  const bytes = new ArrayBuffer(84 + 50);
  const view = new DataView(bytes);
  view.setUint32(80, 1, true);
  const corners = [0, 0, 0, 1, 0, 0, 0, 1, 0];
  corners.forEach((value, index) => view.setFloat32(84 + 12 + index * 4, value, true));
  return bytes;
}

describe("the environment", () => {
  it("knows a mesh file by its extension", () => {
    expect(["a.GLB", "b.gltf", "c.stl", "d.obj", "e.dae", "f"].map(formatOf)).toEqual(["glb", "gltf", "stl", "obj", null, null]);
  });

  it("places, turns by roll, pitch and yaw about fixed axes, and sizes what it builds", async () => {
    const box = { ...placedShape("box", "b1"), position: [1, 2, 3] as [number, number, number], scale: [0.5, 0.4, 0.3] as [number, number, number] };
    const object = await build(box);
    expect(object.position.toArray()).toEqual([1, 2, 3]);
    const size = new THREE.Box3().setFromObject(object).getSize(new THREE.Vector3());
    expect(size.toArray().map((value) => Number(value.toFixed(3)))).toEqual([0.5, 0.4, 0.3]);
    // A yaw of 90° turns X onto Y; a roll of 90° then pitch of 90° turns X onto -Z.
    applyPlacement(object, { ...box, rotation: [0, 0, 90] });
    const x = new THREE.Vector3(1, 0, 0).applyQuaternion(object.quaternion);
    expect(x.toArray().map((value) => Math.round(value))).toEqual([0, 1, 0]);
    applyPlacement(object, { ...box, rotation: [90, 90, 0] });
    const turned = new THREE.Vector3(1, 0, 0).applyQuaternion(object.quaternion);
    expect(turned.toArray().map((value) => Math.round(value) + 0)).toEqual([0, 0, -1]);
  });

  it("builds a mesh from an STL file, at its file's size times its scale", async () => {
    const mesh = { ...placedMesh("part.stl", "stl", oneTriangle(), "m1"), scale: [1000, 1000, 1000] as [number, number, number] };
    const object = await build(mesh);
    const size = new THREE.Box3().setFromObject(object).getSize(new THREE.Vector3());
    expect(size.toArray()).toEqual([1000, 1000, 0]);
  });

  it("keeps what is put until it is removed", async () => {
    const store = memoryStore();
    await store.put(placedShape("sphere", "s1"));
    await store.put({ ...placedShape("sphere", "s1"), name: "Ball" });
    expect((await store.load()).map((each) => each.name)).toEqual(["Ball"]);
    await store.remove("s1");
    expect(await store.load()).toEqual([]);
  });
});
