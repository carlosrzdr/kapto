// SPDX-License-Identifier: Apache-2.0

//! A robot package made from a robot description found elsewhere.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use kapto_core::ErrorCode;
use kapto_robot::{import_package, load_package, load_urdf};
use kapto_testkit::TempDir;

/// A vendor's repository: a URDF in `urdf/` naming a mesh of its own
/// package, a mesh beside it, one a folder up, one that isn't there and one
/// by an absolute path; and a mesh of another package beside the repository.
fn repository(dir: &TempDir) -> PathBuf {
    dir.write("repo/arm_description/meshes/base.stl", b"base");
    dir.write("repo/arm_description/urdf/link.stl", b"link");
    dir.write("repo/arm_description/meshes/upper.dae", b"upper");
    dir.write("repo/gripper_description/meshes/finger.stl", b"finger");
    let absolute = dir.write("elsewhere/part.stl", b"part");
    let mesh =
        |uri: &str| format!(r#"<visual><geometry><mesh filename="{uri}"/></geometry></visual>"#);
    let urdf = format!(
        r#"<robot name="sample_arm">
             <link name="base">{}{}</link>
             <link name="upper">{}{}</link>
             <link name="finger">{}{}{}</link>
             <joint name="shoulder" type="revolute"><parent link="base"/><child link="upper"/><axis xyz="0 0 1"/><limit lower="-1" upper="1" effort="1" velocity="1"/></joint>
             <joint name="wrist" type="continuous"><parent link="upper"/><child link="finger"/><axis xyz="0 0 1"/></joint>
           </robot>"#,
        mesh("package://arm_description/meshes/base.stl"),
        mesh("link.stl"),
        mesh("../meshes/upper.dae"),
        mesh("../meshes/absent.stl"),
        mesh("package://gripper_description/meshes/finger.stl"),
        mesh(&format!("file://{}", absolute.display())),
        // Named twice: copied once.
        mesh("package://gripper_description/meshes/finger.stl"),
    );
    dir.write("repo/arm_description/urdf/arm.urdf", urdf.as_bytes())
}

#[test]
fn a_description_becomes_a_package_whose_urdf_finds_every_file_inside_it_as_written() {
    let dir = TempDir::new("import");
    let urdf = repository(&dir);
    let packages = dir.0.join("packages");
    let imported = import_package(
        &urdf,
        &BTreeMap::new(),
        &[dir.0.join("repo")],
        &packages.join("arm"),
        "arm",
        None,
    )
    .unwrap();

    // The URDF keeps its place relative to the files it names relatively;
    // a package:// file goes under its package's name.
    assert_eq!(imported.urdf, PathBuf::from("urdf/arm.urdf"));
    let mut places: Vec<String> = imported
        .copied
        .iter()
        .map(|(place, _)| place.display().to_string())
        .collect();
    places.sort();
    assert_eq!(
        places,
        [
            "arm_description/meshes/base.stl",
            "gripper_description/meshes/finger.stl",
            "meshes/upper.dae",
            "urdf/link.stl"
        ]
    );
    // What can't be copied is said, each with its own cause.
    let mut left_out: Vec<(ErrorCode, String)> = imported
        .left_out
        .iter()
        .map(|error| (error.code(), error.message()))
        .collect();
    left_out.sort_by_key(|(code, _)| code.id());
    assert_eq!(left_out.len(), 2);
    assert_eq!(left_out[0].0, ErrorCode::PackageImportNotPortable);
    assert!(
        left_out[0].1.contains("elsewhere/part.stl"),
        "{}",
        left_out[0].1
    );
    assert_eq!(left_out[1].0, ErrorCode::RobotMeshUnreadable);
    assert!(
        left_out[1].1.contains("../meshes/absent.stl"),
        "{}",
        left_out[1].1
    );
    assert_eq!(imported.joints, ["shoulder", "wrist"]);

    // It is a package, and its URDF finds what was copied inside it, with
    // nothing to look in but the package itself.
    let package = load_package(&packages.join("arm")).unwrap();
    assert_eq!(package.manifest.id, "arm");
    assert_eq!(package.manifest.name.as_deref(), Some("sample_arm"));
    let model = load_urdf(&package.urdf(), &[]).unwrap();
    let missing: Vec<&str> = model
        .missing_files()
        .map(|(_, file)| file.uri.as_str())
        .collect();
    assert_eq!(missing, ["../meshes/absent.stl"]);
    // Every file found is the package's own, but the one named by an
    // absolute path: still found on this machine, which is why it was said.
    for (_, file) in model.files() {
        let Some(path) = &file.path else { continue };
        assert_eq!(
            path.starts_with(packages.join("arm")),
            !file.uri.starts_with("file://"),
            "{file:?}"
        );
    }
    // Every moving joint is listed in mapping.toml, waiting for its field.
    let mapping = std::fs::read_to_string(packages.join("arm/mapping.toml")).unwrap();
    assert!(
        mapping.contains("# \"shoulder\" = \"\"") && mapping.contains("# \"wrist\" = \"\""),
        "{mapping}"
    );
}

#[test]
fn an_import_never_writes_over_a_folder_and_needs_an_id_kapto_accepts() {
    let dir = TempDir::new("import-refused");
    let urdf = repository(&dir);
    let packages = dir.0.join("packages");
    dir.write("packages/arm/keep.txt", b"mine");

    let error = import_package(
        &urdf,
        &BTreeMap::new(),
        &[],
        &packages.join("arm"),
        "arm",
        None,
    )
    .unwrap_err();
    assert_eq!(error[0].code(), ErrorCode::PackageImportExists);
    assert_eq!(
        std::fs::read(packages.join("arm/keep.txt")).unwrap(),
        b"mine"
    );

    let error = import_package(
        &urdf,
        &BTreeMap::new(),
        &[],
        &packages.join("Arm 2"),
        "Arm 2",
        None,
    )
    .unwrap_err();
    assert_eq!(error[0].code(), ErrorCode::ConfigInvalidValue);
    assert!(!packages.join("Arm 2").exists());
}
