// SPDX-License-Identifier: Apache-2.0

//! Robot packages on disk: loading one, its content hash, and finding every
//! package in a folder.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use kapto_core::ErrorCode;
use kapto_robot::{discover, is_mesh_or_texture, load_package};

/// A folder for one test, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("kapto-package-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MANIFEST: &str = "id = \"bot\"\nversion = \"1.0\"\nurdf = \"urdf/bot.urdf\"\n";

/// Writes `files` into `folder`, in the order given.
fn write(folder: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let path = folder.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

fn package_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("package.toml", MANIFEST),
        (
            "urdf/bot.urdf",
            "<robot name=\"bot\"><link name=\"base\"/></robot>\n",
        ),
        ("streams.toml", "# no streams yet\n"),
        ("mapping.toml", "[joints]\n"),
        ("meshes/b.stl", "solid b\nendsolid b\n"),
        ("meshes/a.stl", "solid a\nendsolid a\n"),
        ("README.md", "# bot\n"),
    ]
}

#[test]
fn the_same_files_give_the_same_hash_whatever_order_they_were_written_in() {
    let scratch = Scratch::new("order");
    let (first, second) = (scratch.0.join("first"), scratch.0.join("second"));
    let mut files = package_files();
    write(&first, &files);
    files.reverse();
    write(&second, &files);
    let (first, second) = (
        load_package(&first).unwrap(),
        load_package(&second).unwrap(),
    );
    assert_eq!(first.hash, second.hash);
    assert_eq!(first.files, second.files);
    let paths: Vec<&str> = first.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "README.md",
            "mapping.toml",
            "meshes/a.stl",
            "meshes/b.stl",
            "package.toml",
            "streams.toml",
            "urdf/bot.urdf"
        ]
    );
    assert_eq!(first.files[0].bytes, 6);
    assert_eq!(first.manifest.id, "bot");
    assert_eq!(first.urdf(), scratch.0.join("first/urdf/bot.urdf"));
}

#[test]
fn the_hash_is_what_sha256sum_gives_for_the_sorted_files() {
    let scratch = Scratch::new("sha256sum");
    write(&scratch.0, &package_files());
    write(
        &scratch.0,
        &[(".git/HEAD", "ref: main\n"), ("meshes/.a.stl.swp", "x")],
    );
    let package = load_package(&scratch.0).unwrap();
    let output = Command::new("sh")
        .arg("-c")
        .arg("find . -type f ! -path '*/.*' | sed 's|^\\./||' | LC_ALL=C sort | xargs sha256sum | sha256sum")
        .current_dir(&scratch.0)
        .output()
        .unwrap();
    assert!(output.status.success());
    let expected = String::from_utf8(output.stdout).unwrap();
    assert_eq!(package.hash, expected.split_whitespace().next().unwrap());
}

#[test]
fn changing_renaming_or_adding_a_file_changes_the_hash_and_dot_files_dont() {
    let scratch = Scratch::new("changes");
    write(&scratch.0, &package_files());
    let hash = || load_package(&scratch.0).unwrap().hash;
    let original = hash();

    write(&scratch.0, &[(".DS_Store", "x"), (".cache/thing", "y")]);
    assert_eq!(hash(), original, "names with a leading dot are left out");

    write(&scratch.0, &[("meshes/a.stl", "solid a\nendsolid a \n")]);
    let changed = hash();
    assert_ne!(changed, original);

    std::fs::rename(scratch.0.join("README.md"), scratch.0.join("NOTES.md")).unwrap();
    let renamed = hash();
    assert_ne!(renamed, changed);

    write(&scratch.0, &[("extra.txt", "")]);
    assert_ne!(hash(), renamed);
}

#[test]
fn a_folder_without_package_toml_is_not_a_package() {
    let scratch = Scratch::new("not");
    let errors = load_package(&scratch.0).unwrap_err();
    assert_eq!(errors[0].code(), ErrorCode::PackageNotFound);
}

#[test]
fn every_file_package_toml_names_must_be_there() {
    let scratch = Scratch::new("missing");
    write(
        &scratch.0,
        &[
            (
                "package.toml",
                "id = \"bot\"\nversion = \"1\"\nurdf = \"bot.urdf\"\nmapping = \"m.toml\"\n",
            ),
            ("streams.toml", ""),
        ],
    );
    let errors = load_package(&scratch.0).unwrap_err();
    let messages: Vec<String> = errors
        .iter()
        .map(|error| {
            assert_eq!(error.code(), ErrorCode::PackageMissingFile);
            error.message()
        })
        .collect();
    assert_eq!(
        messages,
        [
            "A file of the robot package is missing: `urdf` names bot.urdf",
            "A file of the robot package is missing: `mapping` names m.toml"
        ]
    );
}

#[test]
fn a_symbolic_link_in_a_package_is_refused() {
    let scratch = Scratch::new("symlink");
    write(&scratch.0, &package_files());
    std::os::unix::fs::symlink("/etc/hostname", scratch.0.join("meshes/link.stl")).unwrap();
    let errors = load_package(&scratch.0).unwrap_err();
    assert_eq!(errors[0].code(), ErrorCode::PackageUnsafePath);
    assert!(
        errors[0].report().contains("link.stl"),
        "{}",
        errors[0].report()
    );
}

#[test]
fn packages_are_found_in_their_folder_and_each_problem_reported() {
    let scratch = Scratch::new("discover");
    write(&scratch.0.join("b_bot"), &package_files());
    let other: Vec<(&str, &str)> = package_files()
        .into_iter()
        .map(|(path, contents)| {
            if path == "package.toml" {
                (
                    path,
                    "id = \"arm\"\nversion = \"2\"\nurdf = \"urdf/bot.urdf\"\n",
                )
            } else {
                (path, contents)
            }
        })
        .collect();
    write(&scratch.0.join("a_arm"), &other);
    // Same ID as b_bot, found after it.
    write(&scratch.0.join("c_copy"), &package_files());
    // Not a package at all, and a package that's broken.
    write(&scratch.0.join("notes"), &[("todo.txt", "")]);
    write(&scratch.0.join("d_broken"), &[("package.toml", "id = 3\n")]);

    let (found, errors) = discover(&scratch.0);
    let ids: Vec<&str> = found
        .iter()
        .map(|package| package.manifest.id.as_str())
        .collect();
    assert_eq!(ids, ["arm", "bot"]);
    let codes: Vec<ErrorCode> = errors.iter().map(kapto_core::KaptoError::code).collect();
    assert_eq!(
        codes,
        [
            ErrorCode::PackageDuplicateId,
            ErrorCode::ConfigInvalidValue,
            ErrorCode::ConfigInvalidValue,
            ErrorCode::ConfigInvalidValue,
        ]
    );
    assert!(errors[0].message().contains("`bot` is the ID of"));

    let (none, problems) = discover(&scratch.0.join("absent"));
    assert!(none.is_empty() && problems.is_empty());
}

#[test]
fn the_sample_arm_is_a_package() {
    let arm7 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
    let package = load_package(&arm7).unwrap();
    assert_eq!(package.manifest.id, "arm7");
    assert!(package.urdf().is_file());
    assert!(package.streams().is_file());
    assert!(package.mapping().is_file());
}

#[test]
fn a_packages_files_are_read_as_they_were_hashed_and_a_change_since_is_refused() {
    let scratch = Scratch::new("contents");
    write(&scratch.0, &package_files());
    let package = load_package(&scratch.0).unwrap();
    let read = package
        .contents(|file| !is_mesh_or_texture(&file.path))
        .unwrap();
    let paths: Vec<&str> = read.iter().map(|(file, _)| file.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "README.md",
            "mapping.toml",
            "package.toml",
            "streams.toml",
            "urdf/bot.urdf"
        ]
    );
    assert_eq!(read[0].1, b"# bot\n");

    write(
        &scratch.0,
        &[("mapping.toml", "[joints]\nchanged = true\n")],
    );
    let error = package.contents(|_| true).unwrap_err();
    assert_eq!(error.code(), ErrorCode::PackageUnreadable);
    assert!(
        error
            .message()
            .contains("changed after the package was loaded")
    );
}

#[test]
fn meshes_textures_and_their_buffers_are_known_by_their_extension() {
    for mesh in [
        "meshes/a.STL",
        "m/b.dae",
        "c.glb",
        "d.gltf",
        "d.bin",
        "e.obj",
        "e.mtl",
        "t/wood.png",
        "t/x.JPG",
    ] {
        assert!(is_mesh_or_texture(mesh), "{mesh}");
    }
    for other in [
        "package.toml",
        "urdf/bot.urdf",
        "README.md",
        "meshes",
        "stl",
    ] {
        assert!(!is_mesh_or_texture(other), "{other}");
    }
}
