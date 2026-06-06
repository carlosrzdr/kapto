// SPDX-License-Identifier: Apache-2.0

//! The robot as the web UI draws it: each link and its visual shapes,
//! worked out once from the robot model, and each mesh file as plain
//! triangles, so the page reads no mesh format. Specified in
//! `docs/reference/viewer-scene.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::{Bounds, Geometry, Material, MeshFile, RobotModel, Transform, load_mesh, reach};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The colour of shapes whose material doesn't give one.
const NEUTRAL_GREY: [f64; 4] = [0.7, 0.7, 0.72, 1.0];

/// What a mesh's binary form starts with.
pub const MESH_MAGIC: &[u8; 4] = b"KMSH";
/// The version of the mesh's binary form.
pub const MESH_VERSION: u32 = 1;

/// The robot as the page draws it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RobotScene {
    /// The robot's name in its description.
    pub name: String,
    /// The link every other link hangs from, by its name.
    pub root: String,
    /// Every link, in the robot description's order: the order link poses
    /// are sent in as the robot moves.
    pub links: Vec<SceneLink>,
    /// How many mesh files the shapes use, each at
    /// `/api/robot/meshes/{n}`, numbered from 0.
    pub meshes: usize,
    /// A sphere around everywhere the robot's shapes can be with its joints
    /// moved within their limits, to frame the robot once and keep it in
    /// view as it moves; absent for a robot with no shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<SceneReach>,
}

/// A sphere in the root link's frame.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SceneReach {
    /// Its centre: x, y and z, in metres.
    pub center: [f64; 3],
    /// Its radius, in metres.
    pub radius: f64,
}

/// One link and what it looks like.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SceneLink {
    /// Its name.
    pub name: String,
    /// Its visual shapes, in the order the description lists them. A mesh
    /// that isn't there is left out; `/api/robot` warns of it.
    pub visuals: Vec<SceneVisual>,
}

/// One visual shape on a link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SceneVisual {
    /// Where it sits in the link's frame.
    pub origin: Placement,
    /// Its colour: red, green, blue and alpha, from 0 to 1. A mesh part
    /// with a colour of its own uses that instead.
    pub color: [f64; 4],
    /// The shape.
    pub shape: SceneShape,
}

/// A position and an orientation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Placement {
    /// x, y and z, in metres.
    pub translation: [f64; 3],
    /// A unit quaternion: x, y, z, then w.
    pub rotation: [f64; 4],
}

/// A shape, centred on its origin, any length along z.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SceneShape {
    /// A box.
    Box {
        /// Its size along x, y and z, in metres.
        size: [f64; 3],
    },
    /// A cylinder along z.
    Cylinder {
        /// Its radius, in metres.
        radius: f64,
        /// Its length, in metres.
        length: f64,
    },
    /// A cylinder along z with half-spheres on both ends.
    Capsule {
        /// Its radius, in metres.
        radius: f64,
        /// The length of its cylindrical part, in metres.
        length: f64,
    },
    /// A sphere.
    Sphere {
        /// Its radius, in metres.
        radius: f64,
    },
    /// A mesh file's triangles.
    Mesh {
        /// Which mesh: `/api/robot/meshes/{mesh}`.
        mesh: usize,
        /// Scale along x, y and z, applied to the mesh's positions.
        scale: [f64; 3],
    },
}

/// The robot's scene and the mesh files it uses.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// What the page is sent.
    pub robot: RobotScene,
    /// Each mesh file, in the scene's numbering.
    meshes: Vec<PathBuf>,
}

impl Scene {
    /// The scene of `robot`. A mesh file used by several shapes is one
    /// mesh, sent once.
    #[must_use]
    pub fn of(robot: &RobotModel) -> Self {
        Self::of_loaded(robot, &mut HashMap::new())
    }

    /// The scene of `robot` with each of its meshes in the binary form the
    /// page reads, each mesh file read once for both, and why any can't be
    /// read: such a mesh is empty, and left out as the page leaves it out.
    #[must_use]
    pub fn with_meshes(robot: &RobotModel) -> (Self, Vec<Vec<u8>>, Vec<KaptoError>) {
        let mut loaded = HashMap::new();
        let scene = Self::of_loaded(robot, &mut loaded);
        let mut problems = Vec::new();
        let meshes = scene
            .meshes
            .iter()
            .map(|path| {
                let read = loaded.remove(path).unwrap_or_else(|| load_mesh(path));
                match read.and_then(|file| encode(&file)) {
                    Ok(bytes) => bytes,
                    Err(problem) => {
                        problems.push(problem);
                        Vec::new()
                    }
                }
            })
            .collect();
        (scene, meshes, problems)
    }

    /// The scene of `robot`, reading each mesh file not in `loaded` into it.
    fn of_loaded(robot: &RobotModel, loaded: &mut Loaded) -> Self {
        let mut meshes: Vec<PathBuf> = Vec::new();
        let links = robot
            .links
            .iter()
            .map(|link| SceneLink {
                name: link.name.clone(),
                visuals: link
                    .visuals
                    .iter()
                    .filter_map(|visual| {
                        let shape = match &visual.geometry {
                            Geometry::Box { size } => SceneShape::Box { size: *size },
                            Geometry::Cylinder { radius, length } => SceneShape::Cylinder {
                                radius: *radius,
                                length: *length,
                            },
                            Geometry::Capsule { radius, length } => SceneShape::Capsule {
                                radius: *radius,
                                length: *length,
                            },
                            Geometry::Sphere { radius } => SceneShape::Sphere { radius: *radius },
                            Geometry::Mesh { file, scale } => {
                                let path = file.path.as_ref()?;
                                let mesh = meshes.iter().position(|known| known == path);
                                let mesh = mesh.unwrap_or_else(|| {
                                    meshes.push(path.clone());
                                    meshes.len() - 1
                                });
                                SceneShape::Mesh {
                                    mesh,
                                    scale: *scale,
                                }
                            }
                        };
                        Some(SceneVisual {
                            origin: placement(&Transform::from_pose(&visual.origin)),
                            color: color(visual.material.as_ref()),
                            shape,
                        })
                    })
                    .collect(),
            })
            .collect();
        Self {
            robot: RobotScene {
                name: robot.name.clone(),
                root: robot
                    .links
                    .get(robot.root)
                    .map(|link| link.name.clone())
                    .unwrap_or_default(),
                links,
                meshes: meshes.len(),
                reach: reach_of(robot, loaded),
            },
            meshes,
        }
    }

    /// Mesh `n` in its binary form, read from its file now.
    ///
    /// # Errors
    ///
    /// `viewer.mesh_not_found` if the scene has no mesh `n`; the mesh
    /// loader's error if its file can't be read.
    pub fn mesh(&self, n: usize) -> Result<Vec<u8>, KaptoError> {
        let path = self.meshes.get(n).ok_or_else(|| {
            KaptoError::new(ErrorCode::ViewerMeshNotFound).detail(format!(
                "there is no mesh {n}: the robot's shapes use {}",
                self.meshes.len()
            ))
        })?;
        encode(&load_mesh(path)?)
    }
}

/// How far `robot` reaches, each mesh file read once into `loaded` for its
/// bounds. A mesh that can't be read is left out, as the page leaves it
/// out.
fn reach_of(robot: &RobotModel, loaded: &mut Loaded) -> Option<SceneReach> {
    let found = reach(robot, |path: &Path| -> Option<Bounds> {
        let file = loaded
            .entry(path.to_path_buf())
            .or_insert_with(|| load_mesh(path));
        let (low, high) = file.as_ref().ok()?.bounds()?;
        Some((low.map(f64::from), high.map(f64::from)))
    });
    found.ok().flatten().map(|found| SceneReach {
        center: found.center,
        radius: found.radius,
    })
}

/// Each mesh file read, or why it couldn't be.
type Loaded = HashMap<PathBuf, Result<MeshFile, KaptoError>>;

/// `file` in the binary form the page reads; see the module's
/// specification.
///
/// # Errors
///
/// `robot.mesh_invalid` if a part has more vertices or triangles than the
/// form can count.
pub fn encode(file: &MeshFile) -> Result<Vec<u8>, KaptoError> {
    let too_large = || {
        KaptoError::new(ErrorCode::RobotMeshInvalid)
            .detail("a part has more than 4 294 967 295 vertices or triangles".to_owned())
    };
    let count = |n: usize| u32::try_from(n).map_err(|_| too_large());
    let mut bytes = Vec::with_capacity(12 + file.triangle_count() * 12);
    bytes.extend(MESH_MAGIC);
    bytes.extend(MESH_VERSION.to_le_bytes());
    bytes.extend(count(file.meshes.len())?.to_le_bytes());
    for mesh in &file.meshes {
        let vertices = mesh.positions.len();
        let normals = mesh.normals.len() == vertices;
        let colors = mesh.vertex_colors.len() == vertices;
        let flags =
            u32::from(normals) | u32::from(colors) << 1 | u32::from(mesh.color.is_some()) << 2;
        bytes.extend(count(vertices)?.to_le_bytes());
        bytes.extend(count(mesh.triangles.len())?.to_le_bytes());
        bytes.extend(flags.to_le_bytes());
        for channel in mesh.color.unwrap_or_default() {
            bytes.extend(channel.to_le_bytes());
        }
        for value in mesh.positions.iter().flatten() {
            bytes.extend(value.to_le_bytes());
        }
        if normals {
            for value in mesh.normals.iter().flatten() {
                bytes.extend(value.to_le_bytes());
            }
        }
        if colors {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0–255 first"
            )]
            bytes.extend(
                mesh.vertex_colors
                    .iter()
                    .flatten()
                    .map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8),
            );
        }
        for index in mesh.triangles.iter().flatten() {
            bytes.extend(index.to_le_bytes());
        }
    }
    Ok(bytes)
}

fn placement(transform: &Transform) -> Placement {
    Placement {
        translation: transform.translation(),
        rotation: transform.quaternion(),
    }
}

fn color(material: Option<&Material>) -> [f64; 4] {
    material
        .and_then(|material| material.rgba)
        .unwrap_or(NEUTRAL_GREY)
        .map(|channel| channel.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use kapto_robot::Mesh;

    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn a_mesh_is_its_header_then_each_part_with_only_what_it_has() {
        let file = MeshFile {
            meshes: vec![
                Mesh {
                    positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                    normals: vec![[0.0, 0.0, 1.0]; 3],
                    triangles: vec![[0, 1, 2]],
                    color: Some([1.0, 0.5, 0.0, 1.0]),
                    ..Mesh::default()
                },
                Mesh {
                    positions: vec![[0.0, 0.0, 0.0]; 3],
                    vertex_colors: vec![[1.0, 0.0, 0.0, 1.0]; 3],
                    triangles: vec![[2, 1, 0]],
                    ..Mesh::default()
                },
            ],
            ..MeshFile::default()
        };
        let bytes = encode(&file).unwrap();
        assert_eq!(&bytes[..4], MESH_MAGIC);
        assert_eq!((u32_at(&bytes, 4), u32_at(&bytes, 8)), (MESH_VERSION, 2));
        // The first part: 3 vertices, 1 triangle, normals and a colour.
        assert_eq!(
            (u32_at(&bytes, 12), u32_at(&bytes, 16), u32_at(&bytes, 20)),
            (3, 1, 0b101)
        );
        let first = 12 + 28 + 3 * 12 + 3 * 12 + 12;
        // The second: vertex colours as bytes, no normals or colour.
        assert_eq!(u32_at(&bytes, first + 8), 0b010);
        let second = first + 28 + 3 * 12 + 3 * 4 + 12;
        assert_eq!(bytes.len(), second);
        assert_eq!(&bytes[second - 12 - 12..second - 12 - 8], [255, 0, 0, 255]);
        assert_eq!(u32_at(&bytes, second - 12), 2);
    }

    #[test]
    fn colours_out_of_range_are_clamped_and_a_missing_one_is_grey() {
        assert_eq!(
            color(Some(&Material {
                name: None,
                rgba: Some([1.5, -0.2, 0.5, 1.0]),
                texture: None,
            })),
            [1.0, 0.0, 0.5, 1.0]
        );
        assert_eq!(color(None), NEUTRAL_GREY);
    }
}
