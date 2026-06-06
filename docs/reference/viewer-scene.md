# Viewer scene

How the web UI is told what the robot looks like: the scene, as JSON, and
each mesh as plain triangles in a small binary form, so the page never reads
a mesh file format itself. `kapto serve` works the scene out once, from the
robot description it loads when it starts; meshes are read from their files
when asked for. This page is a specification: fields are only ever added,
and the mesh form changes only with its version.

## The scene

`GET /api/robot/scene` answers with the robot's name, its root link, every
link with its visual shapes, how many meshes the shapes use, and how far
the robot reaches. The
[API reference](api.md) lists every field.

```json
{
  "name": "cell",
  "root": "base",
  "links": [
    {
      "name": "base",
      "visuals": [
        {
          "origin": {"translation": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0, 1.0]},
          "color": [1.0, 0.0, 0.0, 1.0],
          "shape": {"kind": "box", "size": [0.1, 0.2, 0.3]}
        }
      ]
    },
    {
      "name": "arm",
      "visuals": [
        {
          "origin": {"translation": [0.0, 0.0, 0.5], "rotation": [0.0, 0.0, 0.0, 1.0]},
          "color": [0.7, 0.7, 0.72, 1.0],
          "shape": {"kind": "mesh", "mesh": 0, "scale": [0.001, 0.001, 0.001]}
        }
      ]
    }
  ],
  "meshes": 1,
  "reach": {"center": [0.0, 0.0, 0.45], "radius": 0.62}
}
```

- **Links** are in the robot description's order. Poses are sent in the
  same order as the robot moves, so a link is known by its position in this
  list.
- **Coordinates** are right-handed, Z up, in metres, as in robot
  descriptions. A visual's `origin` places it in its link's frame:
  `translation` is x, y and z; `rotation` a unit quaternion x, y, z, w.
- **Shapes** are centred on their origin, any length along Z:
  `box` (`size` along x, y and z), `cylinder` and `capsule` (`radius`,
  `length`; a capsule's length is its cylindrical part's, with half-spheres
  on both ends), `sphere` (`radius`) and `mesh` (`mesh`, its number, and
  `scale` along x, y and z, applied to its positions).
- **Colour** is red, green, blue and alpha from 0 to 1: the material's, or
  a neutral grey without one. A mesh part with a colour of its own uses
  that.
- **A mesh file used by several shapes** is one mesh. A shape whose mesh
  file wasn't found is left out of the scene; `/api/robot` warns of it.
- **Reach** is a sphere in the root link's frame (`center` x, y and z,
  `radius`, in metres) around everywhere the robot's shapes can be: each
  shape as the box around it, over 512 joint configurations sampled within
  the joints' limits (a continuous joint a full turn) and every joint at
  zero, with floating, planar and spherical joints at rest. A viewer frames
  it once so the robot stays in view however it moves. It is absent for a
  robot without shapes. Meshes are read once for their bounds when
  `kapto serve` starts.

## A mesh

`GET /api/robot/meshes/{n}` answers with mesh `n` as `application/octet-stream`:
a header, then each part of the file (one per material in it), little-endian
throughout. Every count is an unsigned 32-bit integer, so every array starts
on a four-byte boundary and can be read in place.

| Bytes | Holds |
|---|---|
| 4 | `KMSH` |
| 4 | The form's version: 1 |
| 4 | Parts |

Then, for each part:

| Bytes | Holds |
|---|---|
| 4 | Vertices, `v` |
| 4 | Triangles, `t` |
| 4 | Flags: bit 0, it has normals; bit 1, vertex colours; bit 2, a colour of its own |
| 16 | Its colour: red, green, blue and alpha as 32-bit floats from 0 to 1; zeros without one |
| 12 `v` | Positions: x, y and z as 32-bit floats, in metres once the file's unit is applied |
| 12 `v` | Normals, x, y and z as 32-bit floats, if bit 0 is set |
| 4 `v` | Vertex colours, red, green, blue and alpha as bytes, if bit 1 is set |
| 12 `t` | Triangles: three vertex indices each, as unsigned 32-bit integers |

A file made with Y up is sent as it is, lying on its side; the robot
description can turn it with its visual's `origin`, and `kapto package lint`
reports it.

A mesh the scene doesn't have is refused with `viewer.mesh_not_found`
(404); a mesh file that can't be read any more, with the mesh loader's code
(409).
