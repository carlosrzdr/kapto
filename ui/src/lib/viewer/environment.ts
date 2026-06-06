// What surrounds the robot in the live view: meshes an operator loads and
// simple shapes, each placed, turned and scaled. Display only, and kept in
// this browser: nothing of it reaches Kapto. Positions are in metres in the
// robot's frame, Z up; rotations are roll, pitch and yaw in degrees, as in
// robot descriptions.

import * as THREE from "three";
import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";
import { OBJLoader } from "three/addons/loaders/OBJLoader.js";
import { STLLoader } from "three/addons/loaders/STLLoader.js";

/** The mesh files that can be loaded. */
export type MeshFormat = "glb" | "gltf" | "stl" | "obj";

/** What a thing around the robot is. */
export type Shape = "box" | "cylinder" | "sphere" | "mesh";

/** One thing around the robot. */
export interface Placed {
  id: string;
  name: string;
  shape: Shape;
  /** A mesh's file format and its bytes. */
  format?: MeshFormat;
  file?: ArrayBuffer;
  /** Where its origin is, in metres. */
  position: [number, number, number];
  /** Roll, pitch and yaw, in degrees. */
  rotation: [number, number, number];
  /** Along each axis. A shape's is its size in metres; a mesh's multiplies its file's units. */
  scale: [number, number, number];
  /** `#rrggbb`; a glTF file keeps its own materials. */
  color: string;
}

/** The largest mesh file kept: the browser keeps it whole. */
export const MAX_FILE_BYTES = 50 * 1024 * 1024;

/** The format of a file named `name`, from its extension; `null` if it can't be loaded. */
export function formatOf(name: string): MeshFormat | null {
  const extension = name.toLowerCase().split(".").pop() ?? "";
  return extension === "glb" || extension === "gltf" || extension === "stl" || extension === "obj" ? extension : null;
}

/** A new thing of `shape`, in front of the robot, sized like a small box. */
export function placedShape(shape: Exclude<Shape, "mesh">, id: string): Placed {
  return {
    id,
    name: shape[0]?.toUpperCase() + shape.slice(1),
    shape,
    position: [0.6, 0, 0.1],
    rotation: [0, 0, 0],
    scale: [0.2, 0.2, 0.2],
    color: "#9ca3a8",
  };
}

/**
 * A small work cell to start from, sized for an arm about a metre tall: a
 * table under it, a red cube to pick and a bin to put it in, as in the
 * sample arm's task. Plain shapes, each placed and changed like any other.
 */
export function sampleCell(newId: () => string): Placed[] {
  const shape = (kind: Exclude<Shape, "mesh">, name: string, position: Placed["position"], scale: Placed["scale"], color: string) => ({
    ...placedShape(kind, newId()),
    name,
    position,
    scale,
    color,
  });
  return [
    shape("box", "Table", [0.2, 0, -0.02], [1.4, 1, 0.04], "#4b5156"),
    shape("box", "Red cube", [0.5, 0.3, 0.02], [0.04, 0.04, 0.04], "#dc2626"),
    shape("cylinder", "Bin", [0.45, -0.35, 0.06], [0.2, 0.2, 0.12], "#30353a"),
  ];
}

/** A new mesh from the file `name` in `format`, at the robot's origin at the file's own size. */
export function placedMesh(name: string, format: MeshFormat, file: ArrayBuffer, id: string): Placed {
  return { id, name, shape: "mesh", format, file, position: [0, 0, 0], rotation: [0, 0, 0], scale: [1, 1, 1], color: "#9ca3a8" };
}

/** glTF files are Y up; the scene is Z up. */
const Y_UP_TO_Z_UP = new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(1, 0, 0), Math.PI / 2);

/** Sets `object`'s position, rotation and scale from `placed`. */
export function applyPlacement(object: THREE.Object3D, placed: Placed): void {
  object.position.set(...placed.position);
  const [roll, pitch, yaw] = placed.rotation.map(THREE.MathUtils.degToRad) as [number, number, number];
  // Roll about X, then pitch about Y, then yaw about Z, all fixed axes: as robot descriptions turn.
  object.rotation.set(roll, pitch, yaw, "ZYX");
  object.scale.set(...placed.scale);
}

function material(color: string): THREE.MeshStandardMaterial {
  return new THREE.MeshStandardMaterial({ color: new THREE.Color(color), roughness: 0.8, metalness: 0.05 });
}

/** A unit-sized shape along Z, scaled to its size by its placement. */
function shapeObject(shape: Exclude<Shape, "mesh">, color: string): THREE.Mesh {
  const geometry =
    shape === "box"
      ? new THREE.BoxGeometry(1, 1, 1)
      : shape === "sphere"
        ? new THREE.SphereGeometry(0.5, 24, 16)
        : new THREE.CylinderGeometry(0.5, 0.5, 1, 32).applyQuaternion(Y_UP_TO_Z_UP);
  return new THREE.Mesh(geometry, material(color));
}

/** The mesh in `file`, as three.js objects. */
async function meshObject(format: MeshFormat, file: ArrayBuffer, color: string): Promise<THREE.Object3D> {
  switch (format) {
    case "stl": {
      const geometry = new STLLoader().parse(file);
      geometry.computeVertexNormals();
      return new THREE.Mesh(geometry, material(color));
    }
    case "obj": {
      const object = new OBJLoader().parse(new TextDecoder().decode(file));
      object.traverse((child) => {
        if (child instanceof THREE.Mesh) {
          child.material = material(color);
        }
      });
      return object;
    }
    case "glb":
    case "gltf": {
      // A file whose parts are elsewhere can't be read: only what it holds.
      // Its textures are left out: the page loads images only from Kapto.
      const gltf = await new GLTFLoader().parseAsync(file, "");
      const turned = new THREE.Group();
      turned.quaternion.copy(Y_UP_TO_Z_UP);
      turned.add(gltf.scene);
      return turned;
    }
  }
}

/** Builds the object for `placed`, placed. */
export async function build(placed: Placed): Promise<THREE.Object3D> {
  const inner =
    placed.shape === "mesh"
      ? await meshObject(placed.format ?? "stl", placed.file ?? new ArrayBuffer(0), placed.color)
      : shapeObject(placed.shape, placed.color);
  const outer = new THREE.Group();
  outer.name = placed.id;
  outer.add(inner);
  applyPlacement(outer, placed);
  return outer;
}

/** Sets the colour of everything Kapto made the material of, in `object`. */
export function recolor(object: THREE.Object3D, placed: Placed): void {
  if (placed.format === "glb" || placed.format === "gltf") {
    return;
  }
  object.traverse((child) => {
    if (child instanceof THREE.Mesh && child.material instanceof THREE.MeshStandardMaterial) {
      child.material.color.set(placed.color);
    }
  });
}

/** Where the things around the robot are kept. */
export interface EnvironmentStore {
  load(): Promise<Placed[]>;
  put(placed: Placed): Promise<void>;
  remove(id: string): Promise<void>;
}

/** Kept in memory only, for tests and when the browser keeps nothing. */
export function memoryStore(): EnvironmentStore {
  const kept = new Map<string, Placed>();
  return {
    load: () => Promise.resolve([...kept.values()]),
    put: (placed) => {
      kept.set(placed.id, structuredClone(placed));
      return Promise.resolve();
    },
    remove: (id) => {
      kept.delete(id);
      return Promise.resolve();
    },
  };
}

const DATABASE = "kapto-environment";
const STORE = "placed";

/** Kept in this browser's IndexedDB, which holds mesh files of many megabytes. */
export function browserStore(): EnvironmentStore {
  const opened = new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE, { keyPath: "id" });
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("the browser keeps nothing here"));
  });
  const run = <T>(mode: IDBTransactionMode, work: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> =>
    opened.then(
      (database) =>
        new Promise<T>((resolve, reject) => {
          const request = work(database.transaction(STORE, mode).objectStore(STORE));
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error ?? new Error("the browser couldn't keep it"));
        }),
    );
  return {
    load: () => run("readonly", (store) => store.getAll() as IDBRequest<Placed[]>),
    put: (placed) => run("readwrite", (store) => store.put(placed)).then(() => undefined),
    remove: (id) => run("readwrite", (store) => store.delete(id)).then(() => undefined),
  };
}
