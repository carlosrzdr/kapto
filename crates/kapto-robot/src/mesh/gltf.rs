// SPDX-License-Identifier: Apache-2.0

//! glTF 2.0 (`.gltf` with its buffers, or a single `.glb`).
//!
//! The document is parsed by `gltf`; its buffers are read here: the binary
//! chunk of a `.glb`, a `data:` URI, or a file next to the `.gltf`. Meshes
//! are placed by the transforms of the scene's nodes, as a viewer would show
//! them, and turned from glTF's Y up to the Z up of robot descriptions.

use std::path::Path;

use gltf::buffer::Source;
use gltf::mesh::Mode;
use kapto_core::KaptoError;
use nalgebra::{Matrix3, Matrix4, Point3, Vector3};

use super::{Mesh, MeshFile, base64, invalid, unreadable};

/// A node's placement in the scene.
type Matrix = Matrix4<f32>;

/// glTF is Y up by its specification, robot descriptions Z up: the scene is
/// turned +90° about X, (x, y, z) to (x, -z, y), as rviz turns it, so a part
/// stands as it does there. Written out, so it is exact.
#[rustfmt::skip]
fn z_up() -> Matrix {
    Matrix::new(
        1.0, 0.0, 0.0, 0.0,
        0.0, 0.0, -1.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    )
}

pub(super) fn load(bytes: &[u8], path: &Path) -> Result<MeshFile, KaptoError> {
    let in_file = |error: KaptoError| error.context("file", path.display().to_string());
    let gltf =
        gltf::Gltf::from_slice(bytes).map_err(|error| in_file(invalid(error.to_string())))?;
    let folder = path.parent().unwrap_or(Path::new(""));
    let mut buffers = Vec::new();
    for buffer in gltf.document.buffers() {
        let data = match buffer.source() {
            Source::Bin => gltf
                .blob
                .clone()
                .ok_or_else(|| in_file(invalid("the binary buffer is missing".to_owned())))?,
            Source::Uri(uri) => read_uri(uri, folder).map_err(in_file)?,
        };
        if data.len() < buffer.length() {
            return Err(in_file(invalid(format!(
                "buffer {} has {} bytes but should have {}",
                buffer.index(),
                data.len(),
                buffer.length()
            ))));
        }
        buffers.push(data);
    }

    let mut meshes = Vec::new();
    let scene = gltf
        .document
        .default_scene()
        .or_else(|| gltf.document.scenes().next());
    if let Some(scene) = scene {
        for node in scene.nodes() {
            add_node(&node, z_up(), &buffers, &mut meshes).map_err(in_file)?;
        }
    } else {
        // No scene: every mesh as it is stored.
        for mesh in gltf.document.meshes() {
            add_mesh(&mesh, z_up(), &buffers, &mut meshes).map_err(in_file)?;
        }
    }
    Ok(MeshFile {
        meshes,
        // The glTF specification fixes it.
        unit_metres: Some(1.0),
    })
}

/// A buffer stored outside the document: embedded as base64, or a file
/// relative to the `.gltf`.
fn read_uri(uri: &str, folder: &Path) -> Result<Vec<u8>, KaptoError> {
    if let Some(data) = uri.strip_prefix("data:") {
        let (_, encoded) = data
            .split_once(";base64,")
            .ok_or_else(|| invalid("an embedded buffer isn't base64".to_owned()))?;
        return base64::decode(encoded)
            .ok_or_else(|| invalid("an embedded buffer isn't valid base64".to_owned()));
    }
    if uri.contains("://") {
        return Err(invalid(format!(
            "buffer `{uri}` is not a file next to the mesh; Kapto never downloads files"
        )));
    }
    let path = folder.join(uri);
    std::fs::read(&path)
        .map_err(|error| unreadable(&error).context("buffer", path.display().to_string()))
}

fn add_node(
    node: &gltf::Node<'_>,
    parent: Matrix,
    buffers: &[Vec<u8>],
    meshes: &mut Vec<Mesh>,
) -> Result<(), KaptoError> {
    // glTF stores matrices column by column, as nalgebra builds them.
    let transform = parent * Matrix::from(node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        add_mesh(&mesh, transform, buffers, meshes)?;
    }
    for child in node.children() {
        add_node(&child, transform, buffers, meshes)?;
    }
    Ok(())
}

fn add_mesh(
    mesh: &gltf::Mesh<'_>,
    transform: Matrix,
    buffers: &[Vec<u8>],
    meshes: &mut Vec<Mesh>,
) -> Result<(), KaptoError> {
    let name = mesh.name().unwrap_or_default();
    for primitive in mesh.primitives() {
        if primitive.mode() != Mode::Triangles {
            return Err(invalid(format!(
                "mesh `{name}` is drawn as {:?}; only triangles are supported",
                primitive.mode()
            )));
        }
        let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .ok_or_else(|| invalid(format!("mesh `{name}` has no vertex positions")))?
            .map(|position| transform.transform_point(&Point3::from(position)).into())
            .collect();
        let normal_matrix = normal_matrix(&transform);
        let normals = reader.read_normals().map_or_else(Vec::new, |normals| {
            normals
                .map(|normal| {
                    let normal = normal_matrix * Vector3::from(normal);
                    normal.try_normalize(0.0).unwrap_or(normal).into()
                })
                .collect()
        });
        let vertex_colors = reader
            .read_colors(0)
            .map_or_else(Vec::new, |colors| colors.into_rgba_f32().collect());
        let indices: Vec<u32> = match reader.read_indices() {
            Some(indices) => indices.into_u32().collect(),
            None => (0..u32::try_from(positions.len()).unwrap_or(u32::MAX)).collect(),
        };
        let triangles = indices.as_chunks::<3>().0.to_vec();
        let color = primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_factor();
        meshes.push(Mesh {
            name: name.to_owned(),
            positions,
            normals,
            vertex_colors,
            triangles,
            color: Some(color),
        });
    }
    Ok(())
}

/// What turns normals with `transform`: the inverse transpose of its
/// rotation and scale. A normal must stay perpendicular to its surface, and
/// under a scale that differs between axes the matrix itself would tilt it.
/// A transform that flattens the mesh to nothing has no inverse; its own
/// matrix is the best there is.
fn normal_matrix(transform: &Matrix) -> Matrix3<f32> {
    let linear: Matrix3<f32> = transform.fixed_view::<3, 3>(0, 0).into();
    linear
        .try_inverse()
        .map_or(linear, |inverse| inverse.transpose())
}
