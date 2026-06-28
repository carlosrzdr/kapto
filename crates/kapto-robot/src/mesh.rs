// SPDX-License-Identifier: Apache-2.0

//! Loading the mesh files a robot is drawn with: STL, OBJ, COLLADA (`.dae`)
//! and glTF (`.gltf`, `.glb`), all into one triangle-mesh type, so the
//! viewer and later checks never care which format a robot was made in.
//!
//! STL, OBJ and COLLADA are read by `mesh-loader`, glTF by `gltf`; external
//! glTF buffers and embedded `data:` buffers are read here.

mod base64;
mod gltf;

use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};

/// The meshes in one file, positioned in the file's frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshFile {
    /// Its meshes; a file with several parts or materials has several.
    pub meshes: Vec<Mesh>,
    /// Metres per unit, if the file says; COLLADA and glTF do, STL and OBJ
    /// never do. Positions are already scaled by it.
    pub unit_metres: Option<f32>,
}

/// One triangle mesh with one material.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    /// The mesh's name in the file, possibly empty.
    pub name: String,
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// One normal per vertex, or none if the file has none.
    pub normals: Vec<[f32; 3]>,
    /// One colour per vertex (red, green, blue, alpha, from 0 to 1), or none.
    pub vertex_colors: Vec<[f32; 4]>,
    /// Triangles, as three positions in `positions` each.
    pub triangles: Vec<[u32; 3]>,
    /// The material's colour, if it has one.
    pub color: Option<[f32; 4]>,
}

impl MeshFile {
    /// Triangles in every mesh together.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|mesh| mesh.triangles.len()).sum()
    }

    /// The smallest box holding every vertex, as its lowest and highest
    /// corner; `None` for a file without vertices.
    #[must_use]
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut positions = self.meshes.iter().flat_map(|mesh| &mesh.positions);
        let first = *positions.next()?;
        Some(positions.fold((first, first), |(low, high), point| {
            (
                [0, 1, 2].map(|axis| low[axis].min(point[axis])),
                [0, 1, 2].map(|axis| high[axis].max(point[axis])),
            )
        }))
    }
}

/// Loads the mesh file at `path`, choosing the format by its extension.
///
/// ```
/// use std::path::Path;
///
/// let path = Path::new(env!("CARGO_MANIFEST_DIR"))
///     .join("../../testdata/meshes/handmade/tetrahedron.stl");
/// let tetrahedron = kapto_robot::load_mesh(&path).unwrap();
/// assert_eq!(tetrahedron.triangle_count(), 4);
/// assert_eq!(tetrahedron.bounds(), Some(([0.0; 3], [1.0; 3])));
/// ```
///
/// # Errors
///
/// `robot.mesh_unsupported_format` for an extension other than `.stl`,
/// `.obj`, `.dae`, `.gltf` or `.glb`; `robot.mesh_unreadable` if the file
/// (or a file it refers to) can't be read, or is a Git LFS pointer whose
/// contents were never fetched; `robot.mesh_invalid` if its contents
/// aren't a valid mesh of that format.
pub fn load_mesh(path: &Path) -> Result<MeshFile, KaptoError> {
    let in_file = |error: KaptoError| error.context("file", path.display().to_string());
    let format = Format::of(path)
        .ok_or_else(|| in_file(KaptoError::new(ErrorCode::RobotMeshUnsupportedFormat)))?;
    let bytes = std::fs::read(path).map_err(|error| in_file(unreadable(&error)))?;
    if bytes.starts_with(LFS_POINTER) {
        return Err(in_file(KaptoError::new(ErrorCode::RobotMeshUnreadable).detail(
            "it is a Git LFS pointer, its contents never fetched: run `git lfs pull` in its repository",
        )));
    }
    let file = match format {
        Format::Gltf => gltf::load(&bytes, path)?,
        Format::Stl | Format::Obj | Format::Collada => {
            let scene = read_scene(&bytes, path)
                .or_else(|error| match format {
                    Format::Stl => binary_stl(&bytes, path).ok_or(error),
                    _ => Err(error),
                })
                .map_err(|error| in_file(invalid(error)))?;
            from_scene(scene, format, &bytes)
        }
    };
    check(&file).map_err(|detail| in_file(invalid(detail)))?;
    Ok(file)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Stl,
    Obj,
    Collada,
    Gltf,
}

impl Format {
    fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "stl" => Some(Self::Stl),
            "obj" => Some(Self::Obj),
            "dae" => Some(Self::Collada),
            "gltf" | "glb" => Some(Self::Gltf),
            _ => None,
        }
    }
}

fn from_scene(scene: mesh_loader::Scene, format: Format, bytes: &[u8]) -> MeshFile {
    let mut materials = scene.materials.into_iter();
    let meshes = scene
        .meshes
        .into_iter()
        .map(|mesh| {
            let color = materials.next().and_then(|material| material.color.diffuse);
            let [vertex_colors, ..] = mesh.colors;
            Mesh {
                name: mesh.name,
                positions: mesh.vertices,
                normals: mesh.normals,
                vertex_colors,
                triangles: mesh.faces,
                color,
            }
        })
        .collect();
    let unit_metres = if format == Format::Collada {
        collada_unit(bytes)
    } else {
        None
    };
    MeshFile {
        meshes,
        unit_metres,
    }
}

/// The unit a COLLADA file declares in its `<asset>`, which comes first:
/// only the start of the file is searched. Its `<up_axis>` is left alone, as
/// rviz leaves it: robot descriptions are made to look right without it.
fn collada_unit(bytes: &[u8]) -> Option<f32> {
    let start = String::from_utf8_lossy(&bytes[..bytes.len().min(64 * 1024)]);
    let asset = start
        .split_once("</asset>")
        .map_or(start.as_ref(), |(asset, _)| asset);
    let unit = asset.split_once("<unit").and_then(|(_, rest)| {
        let (_, value) = rest.split_once("meter=\"")?;
        value.split_once('"')?.0.trim().parse().ok()
    });
    unit.or(Some(1.0))
}

/// What's wrong with the meshes, if anything: a triangle pointing past the
/// vertices, a position that isn't a finite number, or per-vertex data of
/// the wrong length.
fn check(file: &MeshFile) -> Result<(), String> {
    for (index, mesh) in file.meshes.iter().enumerate() {
        let name = if mesh.name.is_empty() {
            format!("mesh {}", index + 1)
        } else {
            format!("mesh `{}`", mesh.name)
        };
        let vertices = mesh.positions.len();
        if mesh
            .triangles
            .iter()
            .flatten()
            .any(|&vertex| vertex as usize >= vertices)
        {
            return Err(format!(
                "{name} has a triangle using a vertex it doesn't have ({vertices} vertices)"
            ));
        }
        if mesh
            .positions
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(format!("{name} has a vertex position that isn't a number"));
        }
        let lengths_match = [mesh.normals.len(), mesh.vertex_colors.len()]
            .iter()
            .all(|&len| len == 0 || len == vertices);
        if !lengths_match {
            return Err(format!(
                "{name} has normals or colours for a different number of vertices than it has"
            ));
        }
    }
    Ok(())
}

/// What a file Git LFS stands in for begins with when its contents were
/// never fetched.
const LFS_POINTER: &[u8] = b"version https://git-lfs.github.com/spec/v1";

/// Reads `bytes` with `mesh-loader`. A malformed file can make it panic
/// (e.g. a COLLADA index past the end of its positions); that is the
/// file's fault, so it is refused like any other invalid file instead of
/// taking down the process that asked for it.
fn read_scene(bytes: &[u8], path: &Path) -> Result<mesh_loader::Scene, String> {
    std::panic::catch_unwind(|| mesh_loader::Loader::default().load_from_slice(bytes, path))
        .map_err(|panic| {
            let said = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("no reason given");
            format!("the mesh reader failed on it: {said}")
        })?
        .map_err(|error| error.to_string())
}

/// `bytes` read as a binary STL whose 80-byte header begins with `solid`,
/// as some exporters write it, which makes it look like an ASCII STL.
/// `None` unless its size is exactly that of a binary STL with the
/// triangle count it gives.
fn binary_stl(bytes: &[u8], path: &Path) -> Option<mesh_loader::Scene> {
    let count = u32::from_le_bytes(bytes.get(80..84)?.try_into().ok()?);
    let expected = usize::try_from(count)
        .ok()?
        .checked_mul(50)?
        .checked_add(84)?;
    if expected != bytes.len() {
        return None;
    }
    // The header is free text: without its `solid` only the binary reading is left.
    let solid = bytes[..80].windows(5).position(|word| word == b"solid")?;
    let mut renamed = bytes.to_vec();
    renamed[solid..solid + 5].copy_from_slice(b"SOLID");
    read_scene(&renamed, path).ok()
}

fn unreadable(error: &std::io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::RobotMeshUnreadable).detail(error.to_string())
}

fn invalid(detail: String) -> KaptoError {
    KaptoError::new(ErrorCode::RobotMeshInvalid).detail(detail)
}
