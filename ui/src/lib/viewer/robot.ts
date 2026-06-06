// The robot as three.js objects: a group per link, placed by the stream's
// poses, holding the link's shapes where the scene puts them. Robot
// descriptions are Z up and so is this scene; three.js's own shapes are
// along Y, so cylinders and capsules are turned onto Z.

import * as THREE from "three";

import type { Placement, RobotScene, SceneShape } from "../api-types";
import type { MeshPart } from "./mesh";

/** The robot: its root object, and each link's group in the scene's link order. */
export interface Robot {
  root: THREE.Group;
  links: THREE.Group[];
}

/** Turns three.js's Y-along shapes onto Z, as the scene's are. */
const ONTO_Z = new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(1, 0, 0), Math.PI / 2);

function place(object: THREE.Object3D, placement: Placement) {
  const [x = 0, y = 0, z = 0] = placement.translation;
  const [qx = 0, qy = 0, qz = 0, qw = 1] = placement.rotation;
  object.position.set(x, y, z);
  object.quaternion.set(qx, qy, qz, qw);
}

function material(color: number[], vertexColors = false): THREE.MeshStandardMaterial {
  const [r = 0.7, g = 0.7, b = 0.72, a = 1] = color;
  return new THREE.MeshStandardMaterial({
    color: new THREE.Color(r, g, b),
    opacity: a,
    transparent: a < 1,
    vertexColors,
    roughness: 0.6,
    metalness: 0.1,
  });
}

/** The three.js geometry of a shape that isn't a mesh, along Z. */
function geometry(shape: Exclude<SceneShape, { kind: "mesh" }>): THREE.BufferGeometry {
  switch (shape.kind) {
    case "box": {
      const [x = 0, y = 0, z = 0] = shape.size;
      return new THREE.BoxGeometry(x, y, z);
    }
    case "sphere":
      return new THREE.SphereGeometry(shape.radius, 24, 16);
    case "cylinder":
      return new THREE.CylinderGeometry(shape.radius, shape.radius, shape.length, 24).applyQuaternion(ONTO_Z);
    case "capsule":
      return new THREE.CapsuleGeometry(shape.radius, shape.length, 8, 24).applyQuaternion(ONTO_Z);
  }
}

/** A mesh part as three.js geometry. */
export function meshGeometry(part: MeshPart): THREE.BufferGeometry {
  const built = new THREE.BufferGeometry();
  built.setAttribute("position", new THREE.BufferAttribute(part.positions, 3));
  built.setIndex(new THREE.BufferAttribute(part.indices, 1));
  if (part.normals) {
    built.setAttribute("normal", new THREE.BufferAttribute(part.normals, 3));
  } else {
    built.computeVertexNormals();
  }
  if (part.colors) {
    built.setAttribute("color", new THREE.BufferAttribute(part.colors, 4, true));
  }
  return built;
}

/**
 * The robot of `scene`, its meshes read with `mesh`. A mesh that can't be
 * read is left out, and `missing` told; the rest of the robot is shown.
 */
export async function buildRobot(
  scene: RobotScene,
  mesh: (n: number) => Promise<MeshPart[]>,
  missing: (n: number, error: unknown) => void = () => {},
): Promise<Robot> {
  const root = new THREE.Group();
  root.name = scene.name;
  const meshes = new Map<number, Promise<MeshPart[] | null>>();
  const parts = (n: number) => {
    let read = meshes.get(n);
    if (read === undefined) {
      read = mesh(n).catch((error: unknown) => {
        missing(n, error);
        return null;
      });
      meshes.set(n, read);
    }
    return read;
  };
  const links = await Promise.all(
    scene.links.map(async (link) => {
      const group = new THREE.Group();
      group.name = link.name;
      for (const visual of link.visuals) {
        const holder = new THREE.Group();
        place(holder, visual.origin);
        if (visual.shape.kind === "mesh") {
          const [x = 1, y = 1, z = 1] = visual.shape.scale;
          holder.scale.set(x, y, z);
          for (const part of (await parts(visual.shape.mesh)) ?? []) {
            holder.add(new THREE.Mesh(meshGeometry(part), material(part.color ?? visual.color, part.colors !== null)));
          }
        } else {
          holder.add(new THREE.Mesh(geometry(visual.shape), material(visual.color)));
        }
        group.add(holder);
      }
      root.add(group);
      return group;
    }),
  );
  let drawn = false;
  root.traverse((object) => (drawn ||= object instanceof THREE.Mesh));
  if (!drawn) {
    // Nothing could be drawn (no visuals, or every mesh missing): a dot
    // at each link still shows the robot is there and moving.
    const dot = new THREE.SphereGeometry(0.015, 12, 8);
    const look = material([0.9, 0.62, 0.2, 1]);
    for (const link of links) {
      link.add(new THREE.Mesh(dot, look));
    }
  }
  return { root, links };
}

/**
 * How far from the centre of a sphere of `radius` a camera with a vertical
 * field of view of `fovDegrees` and width over height `aspect` must be to
 * see all of it: by the narrower of its two fields of view, so a robot
 * reaching sideways stays in a tall view.
 */
export function viewingDistance(radius: number, fovDegrees: number, aspect: number): number {
  const vertical = THREE.MathUtils.degToRad(fovDegrees);
  const horizontal = 2 * Math.atan(Math.tan(vertical / 2) * aspect);
  return radius / Math.sin(Math.min(vertical, horizontal) / 2);
}

/**
 * Places each link as `poses` says: seven numbers a link, relative to the
 * root link, in the scene's link order. Returns whether they fitted.
 */
export function applyPoses(links: THREE.Object3D[], poses: Float32Array): boolean {
  if (poses.length !== links.length * 7) {
    return false;
  }
  links.forEach((link, index) => {
    link.position.fromArray(poses, index * 7);
    link.quaternion.fromArray(poses, index * 7 + 3);
  });
  return true;
}
