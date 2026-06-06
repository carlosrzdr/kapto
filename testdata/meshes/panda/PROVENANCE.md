# Franka Emika Panda meshes

- **Files:** `finger.dae` (visual mesh of a finger) and `hand.stl` (collision
  mesh of the hand), unchanged.
- **Source:** <https://github.com/Gepetto/example-robot-data>, files
  `robots/panda_description/meshes/visual/finger.dae` and
  `robots/panda_description/meshes/collision/hand.stl`, branch `master`;
  SHA-256 `572bae42ffcde0e88c449f2f9212f8bb4f4c9946a85c52970eb2fb9bb51baa84` and
  `94493e94f30fe940f2c8ca2f155c3bbe67bbff406d3edf5e261670d2f0f6e2ed`.
- **Licence:** BSD-3-Clause, see `LICENSE` in this folder. The meshes come
  from the `franka_description` package of `franka_ros` (Apache-2.0).
- **Why they are here:** real meshes Kapto didn't make: a COLLADA file with
  two materials exported from a CAD tool, and a binary STL from Blender.
- **Reference values** in the tests come from the files themselves, not
  from Kapto: the triangle counts written in `finger.dae` (`<triangles
  count="262">` and `count="362"`), and the triangle count and bounding box
  of `hand.stl` decoded with Python's `struct` module.
