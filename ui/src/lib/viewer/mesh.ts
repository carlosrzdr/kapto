// A mesh in the binary form `/api/robot/meshes/{n}` sends, as the viewer
// scene's specification gives it: a header, then each part with its
// counts, flags and colour, its positions, optional normals and vertex
// colours, and its triangles. Every array starts on a four-byte boundary,
// so it is read in place.

/** One part of a mesh: one material's triangles. */
export interface MeshPart {
  /** x, y, z per vertex, in metres. */
  positions: Float32Array;
  /** x, y, z per vertex, or none. */
  normals: Float32Array | null;
  /** Red, green, blue, alpha per vertex, 0 to 255, or none. */
  colors: Uint8Array | null;
  /** Three vertex indices per triangle. */
  indices: Uint32Array;
  /** The part's own colour, 0 to 1, or none. */
  color: [number, number, number, number] | null;
}

const MAGIC = "KMSH";
const VERSION = 1;

/** The parts of the mesh in `buffer`; throws if it isn't one this page reads. */
export function decodeMesh(buffer: ArrayBuffer): MeshPart[] {
  const view = new DataView(buffer);
  const magic = String.fromCharCode(...new Uint8Array(buffer, 0, 4));
  const version = view.getUint32(4, true);
  if (magic !== MAGIC || version !== VERSION) {
    throw new Error(`not a mesh this page reads: ${magic} version ${version}`);
  }
  const parts: MeshPart[] = [];
  let at = 12;
  for (let part = 0, count = view.getUint32(8, true); part < count; part++) {
    const vertices = view.getUint32(at, true);
    const triangles = view.getUint32(at + 4, true);
    const flags = view.getUint32(at + 8, true);
    const color: [number, number, number, number] = [0, 1, 2, 3].map((channel) =>
      view.getFloat32(at + 12 + channel * 4, true),
    ) as [number, number, number, number];
    at += 28;
    const positions = new Float32Array(buffer, at, vertices * 3);
    at += vertices * 12;
    let normals: Float32Array | null = null;
    if (flags & 1) {
      normals = new Float32Array(buffer, at, vertices * 3);
      at += vertices * 12;
    }
    let colors: Uint8Array | null = null;
    if (flags & 2) {
      colors = new Uint8Array(buffer, at, vertices * 4);
      at += vertices * 4;
    }
    const indices = new Uint32Array(buffer, at, triangles * 3);
    at += triangles * 12;
    parts.push({ positions, normals, colors, indices, color: flags & 4 ? color : null });
  }
  return parts;
}
