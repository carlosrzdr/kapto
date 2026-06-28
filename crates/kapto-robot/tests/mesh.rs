// SPDX-License-Identifier: Apache-2.0

//! Loading mesh files: real robot meshes, one small file per format with
//! known contents, and every way a mesh file can be refused.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_core::ErrorCode;
use kapto_robot::load_mesh;
use kapto_testkit::TempDir;

fn testdata(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/meshes")
        .join(path)
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1e-6)
}

#[test]
fn a_real_collada_mesh_loads_with_its_two_materials() {
    let finger = load_mesh(&testdata("panda/finger.dae")).unwrap();
    // In the order the scene places them, which needn't be file order.
    let mut triangles: Vec<_> = finger
        .meshes
        .iter()
        .map(|mesh| mesh.triangles.len())
        .collect();
    triangles.sort_unstable();
    assert_eq!(triangles, [262, 362]);
    assert_eq!(finger.triangle_count(), 624);
    assert!(finger.meshes.iter().all(|mesh| mesh.color.is_some()));
    assert_eq!(finger.unit_metres, Some(1.0));
}

#[test]
fn a_real_binary_stl_loads_with_its_size_in_metres() {
    let hand = load_mesh(&testdata("panda/hand.stl")).unwrap();
    assert_eq!(hand.triangle_count(), 200);
    let (low, high) = hand.bounds().unwrap();
    assert!(
        close(low, [-0.031_635_918, -0.103_989_72, -0.025_924_802]),
        "{low:?}"
    );
    assert!(
        close(high, [0.031_615_783, 0.100_425_88, 0.065_962_15]),
        "{high:?}"
    );
    assert_eq!(hand.unit_metres, None);
}

#[test]
fn an_ascii_stl_loads_every_facet() {
    let tetrahedron = load_mesh(&testdata("handmade/tetrahedron.stl")).unwrap();
    assert_eq!(tetrahedron.triangle_count(), 4);
    assert_eq!(tetrahedron.bounds(), Some(([0.0; 3], [1.0; 3])));
}

#[test]
fn an_obj_loads_with_the_colour_of_its_material_file() {
    let plate = load_mesh(&testdata("handmade/plate.obj")).unwrap();
    assert_eq!(plate.triangle_count(), 2);
    assert_eq!(plate.bounds(), Some(([0.0; 3], [2.0, 1.0, 0.0])));
    let mesh = &plate.meshes[0];
    assert_eq!(
        mesh.color.map(|[r, g, b, _]| [r, g, b]),
        Some([1.0, 0.0, 0.0])
    );
    assert!(
        mesh.normals
            .iter()
            .all(|&normal| close(normal, [0.0, 0.0, 1.0]))
    );
}

#[test]
fn a_collada_file_in_millimetres_is_scaled_to_metres_and_left_as_drawn_whatever_its_up_axis() {
    // It says Y up; rviz leaves it as drawn, and so does Kapto.
    let triangle = load_mesh(&testdata("handmade/millimetre_triangle.dae")).unwrap();
    assert_eq!(triangle.unit_metres, Some(0.001));
    let (low, high) = triangle.bounds().unwrap();
    assert!(
        close(low, [0.0; 3]) && close(high, [1.0, 1.0, 0.0]),
        "{low:?} {high:?}"
    );
}

/// One triangle: positions (0,0,0), (1,0,0), (0,1,0) as f32, then indices
/// 0, 1, 2 (or `indices`) as u16.
fn triangle_buffer(indices: [u16; 3]) -> Vec<u8> {
    let positions = [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    let mut bytes: Vec<u8> = positions.iter().flat_map(|v| v.to_le_bytes()).collect();
    bytes.extend(indices.iter().flat_map(|i| i.to_le_bytes()));
    bytes
}

/// A glTF document drawing the triangle through two nodes: the outer one
/// moves it 5 m up (glTF's Y), the inner one doubles its size. `buffer` is the
/// buffer's JSON; `mode` the primitive's drawing mode (4 = triangles).
fn gltf_json(buffer: &str, mode: u32) -> String {
    format!(
        r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [{{ "nodes": [0] }}],
  "nodes": [
    {{ "translation": [0, 5, 0], "children": [1] }},
    {{ "scale": [2, 2, 2], "mesh": 0 }}
  ],
  "meshes": [{{ "name": "triangle", "primitives": [{{ "attributes": {{ "POSITION": 0 }}, "indices": 1, "material": 0, "mode": {mode} }}] }}],
  "materials": [{{ "pbrMetallicRoughness": {{ "baseColorFactor": [0, 0.5, 1, 1] }} }}],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0] }},
    {{ "bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR" }}
  ],
  "bufferViews": [
    {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }},
    {{ "buffer": 0, "byteOffset": 36, "byteLength": 6 }}
  ],
  "buffers": [{buffer}]
}}"#
    )
}

fn data_uri(bytes: &[u8]) -> String {
    const SYMBOLS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0_u32, |group, (i, &byte)| {
            group | u32::from(byte) << (16 - 8 * i)
        });
        for i in 0..=chunk.len() {
            text.push(char::from(SYMBOLS[(group >> (18 - 6 * i) & 63) as usize]));
        }
    }
    format!(
        r#"{{ "byteLength": {}, "uri": "data:application/octet-stream;base64,{text}" }}"#,
        bytes.len()
    )
}

/// The triangle as glTF puts it, moved up 5 m and twice as large, then
/// turned from glTF's Y up to Z up as rviz turns it: standing, its glTF up
/// edge pointing up.
fn assert_placed_triangle(file: &kapto_robot::MeshFile) {
    let mesh = &file.meshes[0];
    assert_eq!(mesh.name, "triangle");
    assert_eq!(
        mesh.positions,
        [[0.0, 0.0, 5.0], [2.0, 0.0, 5.0], [0.0, 0.0, 7.0]]
    );
    assert_eq!(mesh.triangles, [[0, 1, 2]]);
    assert_eq!(mesh.color, Some([0.0, 0.5, 1.0, 1.0]));
    assert_eq!(file.unit_metres, Some(1.0));
}

#[test]
fn a_gltf_with_an_embedded_buffer_is_placed_by_its_nodes() {
    let dir = TempDir::new("embedded");
    let json = gltf_json(&data_uri(&triangle_buffer([0, 1, 2])), 4);
    let file = load_mesh(&dir.write("triangle.gltf", json.as_bytes())).unwrap();
    assert_placed_triangle(&file);
}

#[test]
fn normals_stay_perpendicular_to_a_surface_stretched_more_along_one_axis() {
    // A triangle sloping at 45° with its normal, stretched three times
    // taller: the surface gets steeper, so its normal leans further over.
    let positions = [0.0_f32, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0];
    let half = std::f32::consts::FRAC_1_SQRT_2;
    let normals = [-half, 0.0, half, -half, 0.0, half, -half, 0.0, half];
    let bytes: Vec<u8> = positions
        .iter()
        .chain(&normals)
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let json = format!(
        r#"{{
  "asset": {{ "version": "2.0" }},
  "scene": 0,
  "scenes": [{{ "nodes": [0] }}],
  "nodes": [{{ "scale": [1, 1, 3], "mesh": 0 }}],
  "meshes": [{{ "primitives": [{{ "attributes": {{ "POSITION": 0, "NORMAL": 1 }} }}] }}],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 1] }},
    {{ "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3" }}
  ],
  "bufferViews": [
    {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }},
    {{ "buffer": 0, "byteOffset": 36, "byteLength": 36 }}
  ],
  "buffers": [{}]
}}"#,
        data_uri(&bytes)
    );
    let dir = TempDir::new("stretched");
    let file = load_mesh(&dir.write("stretched.gltf", json.as_bytes())).unwrap();
    let mesh = &file.meshes[0];
    // The stretched triangle's edges, and the normal perpendicular to both.
    let [a, b, c] = [mesh.positions[0], mesh.positions[1], mesh.positions[2]];
    let edges = [
        [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
        [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
    ];
    // (-3, 0, 1) in glTF's frame, turned to Z up with the rest.
    let length = 10.0_f32.sqrt();
    for normal in &mesh.normals {
        assert!(
            close(*normal, [-3.0 / length, -1.0 / length, 0.0]),
            "{normal:?}"
        );
        for edge in edges {
            let dot: f32 = normal.iter().zip(edge).map(|(n, e)| n * e).sum();
            assert!(
                dot.abs() < 1e-6,
                "{normal:?} isn't perpendicular to {edge:?}"
            );
        }
    }
}

#[test]
fn a_gltf_with_its_buffer_in_a_separate_file_loads() {
    let dir = TempDir::new("separate");
    dir.write("triangle.bin", &triangle_buffer([0, 1, 2]));
    let json = gltf_json(r#"{ "byteLength": 42, "uri": "triangle.bin" }"#, 4);
    let file = load_mesh(&dir.write("triangle.gltf", json.as_bytes())).unwrap();
    assert_placed_triangle(&file);
}

#[test]
fn a_binary_glb_loads() {
    let dir = TempDir::new("glb");
    let mut json = gltf_json(r#"{ "byteLength": 42 }"#, 4).into_bytes();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut bin = triangle_buffer([0, 1, 2]);
    bin.resize(bin.len().next_multiple_of(4), 0);
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut glb = Vec::new();
    glb.extend(b"glTF");
    glb.extend(2_u32.to_le_bytes());
    glb.extend(u32::try_from(total).unwrap().to_le_bytes());
    glb.extend(u32::try_from(json.len()).unwrap().to_le_bytes());
    glb.extend(b"JSON");
    glb.extend(&json);
    glb.extend(u32::try_from(bin.len()).unwrap().to_le_bytes());
    glb.extend(b"BIN\0");
    glb.extend(&bin);
    let file = load_mesh(&dir.write("triangle.glb", &glb)).unwrap();
    assert_placed_triangle(&file);
}

fn error_code(path: &Path) -> ErrorCode {
    load_mesh(path).unwrap_err().code()
}

#[test]
fn broken_gltf_files_are_refused_with_their_cause() {
    let dir = TempDir::new("broken-gltf");
    let missing_buffer = gltf_json(r#"{ "byteLength": 42, "uri": "nowhere.bin" }"#, 4);
    assert_eq!(
        error_code(&dir.write("a.gltf", missing_buffer.as_bytes())),
        ErrorCode::RobotMeshUnreadable
    );
    let remote = gltf_json(
        r#"{ "byteLength": 42, "uri": "https://example.com/a.bin" }"#,
        4,
    );
    let error = load_mesh(&dir.write("b.gltf", remote.as_bytes())).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotMeshInvalid);
    assert!(
        error.message().contains("never downloads"),
        "{}",
        error.message()
    );
    let lines = gltf_json(&data_uri(&triangle_buffer([0, 1, 2])), 1);
    assert_eq!(
        error_code(&dir.write("c.gltf", lines.as_bytes())),
        ErrorCode::RobotMeshInvalid
    );
    let past_the_end = gltf_json(&data_uri(&triangle_buffer([0, 1, 7])), 4);
    let error = load_mesh(&dir.write("d.gltf", past_the_end.as_bytes())).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotMeshInvalid);
    assert!(
        error.message().contains("a vertex it doesn't have"),
        "{}",
        error.message()
    );
    let short = gltf_json(&data_uri(&triangle_buffer([0, 1, 2])[..30]), 4);
    assert_eq!(
        error_code(&dir.write("e.gltf", short.as_bytes())),
        ErrorCode::RobotMeshInvalid
    );
    assert_eq!(
        error_code(&dir.write("f.gltf", b"{ not json")),
        ErrorCode::RobotMeshInvalid
    );
}

#[test]
fn broken_stl_obj_and_collada_files_are_refused_as_invalid() {
    let dir = TempDir::new("broken");
    let hand = std::fs::read(testdata("panda/hand.stl")).unwrap();
    assert_eq!(
        error_code(&dir.write("cut.stl", &hand[..500])),
        ErrorCode::RobotMeshInvalid
    );
    assert_eq!(
        error_code(&dir.write("bad.obj", b"v 0 0 0\nv 1 0 0\nf 1 2 9\n")),
        ErrorCode::RobotMeshInvalid
    );
    assert_eq!(
        error_code(&dir.write("bad.dae", b"<COLLADA><library_geometries>")),
        ErrorCode::RobotMeshInvalid
    );
}

#[test]
fn a_binary_stl_whose_header_starts_with_solid_loads_as_binary() {
    let dir = TempDir::new("solid-header");
    // Ten triangles, as an exporter that writes `solid` into a binary
    // header makes them: the count's first byte, 10, reads as a newline
    // ending the "solid" line, so the file looks like an ASCII STL.
    let mut stl = vec![b' '; 80];
    stl[..12].copy_from_slice(b"solid thigh ");
    stl.extend_from_slice(&10_u32.to_le_bytes());
    for n in 0..10_u8 {
        let x = f32::from(n);
        let mut triangle = [
            0.0_f32,
            0.0,
            1.0,
            x,
            0.0,
            0.0,
            x + 1.0,
            0.0,
            0.0,
            x,
            1.0,
            0.0,
        ]
        .map(f32::to_le_bytes)
        .concat();
        triangle.extend_from_slice(&[0, 0]);
        stl.extend_from_slice(&triangle);
    }
    let file = load_mesh(&dir.write("thigh.stl", &stl)).unwrap();
    assert_eq!(file.triangle_count(), 10);
    assert_eq!(file.bounds(), Some(([0.0, 0.0, 0.0], [10.0, 1.0, 0.0])));
    // A text STL that is truly broken is still refused.
    assert_eq!(
        error_code(&dir.write("text.stl", b"solid a\nfacet nonsense\n")),
        ErrorCode::RobotMeshInvalid
    );
}

#[test]
fn a_collada_file_that_makes_the_reader_panic_is_refused_as_invalid() {
    let dir = TempDir::new("panicking-collada");
    let triangle = std::fs::read_to_string(testdata("handmade/millimetre_triangle.dae")).unwrap();
    let past_the_end = triangle.replace("<p>0 1 2</p>", "<p>0 1 3</p>");
    let error = load_mesh(&dir.write("past.dae", past_the_end.as_bytes())).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotMeshInvalid);
    assert!(
        error.message().contains("the mesh reader failed on it"),
        "{}",
        error.message()
    );
}

#[test]
fn a_git_lfs_pointer_is_unreadable_and_says_to_fetch_it() {
    let dir = TempDir::new("lfs-pointer");
    let pointer = b"version https://git-lfs.github.com/spec/v1\noid sha256:bf02b5\nsize 132184\n";
    let error = load_mesh(&dir.write("knee.stl", pointer)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotMeshUnreadable);
    assert!(
        error.message().contains("git lfs pull"),
        "{}",
        error.message()
    );
}

#[test]
fn a_mesh_that_cannot_be_read_or_has_an_unknown_format_is_refused() {
    assert_eq!(
        error_code(Path::new("/no/such/mesh.stl")),
        ErrorCode::RobotMeshUnreadable
    );
    let error = load_mesh(Path::new("scan.ply")).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotMeshUnsupportedFormat);
    assert!(
        error
            .context_entries()
            .contains(&("file", "scan.ply".to_owned()))
    );
}
