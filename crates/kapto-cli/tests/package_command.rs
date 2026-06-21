// SPDX-License-Identifier: Apache-2.0

//! `kapto package`, run as a real process on packages in a folder.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A packages folder for one test, removed when dropped.
struct Packages(PathBuf);

impl Packages {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("kapto-packages-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// A copy of the sample arm, under `folder`.
    fn add_arm7(&self, folder: &str) -> PathBuf {
        let arm7 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
        let target = self.0.join(folder);
        for file in [
            "package.toml",
            "streams.toml",
            "mapping.toml",
            "urdf/arm7.urdf",
            "README.md",
        ] {
            let to = target.join(file);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(arm7.join(file), to).unwrap();
        }
        target
    }
}

impl Drop for Packages {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn kapto(packages: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(args)
        .arg("--packages-dir")
        .arg(packages)
        .env_clear()
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn list_and_show_name_each_package_by_id_version_and_hash() {
    let packages = Packages::new("list");
    let folder = packages.add_arm7("arm7");
    let output = kapto(&packages.0, &["package", "list"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let listed = text(&output.stdout);
    let row: Vec<&str> = listed.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(&row[..2], ["arm7", "0.1.0"]);
    assert_eq!(row[2].len(), 12);

    let by_id = kapto(&packages.0, &["package", "show", "arm7"]);
    let by_folder = kapto(&packages.0, &["package", "show", folder.to_str().unwrap()]);
    assert!(by_id.status.success() && by_folder.status.success());
    assert_eq!(by_id.stdout, by_folder.stdout);
    let shown = text(&by_id.stdout);
    assert!(
        shown.starts_with("arm7 0.1.0: Sample seven-joint arm (Kapto)\n"),
        "{shown}"
    );
    let hash = shown
        .lines()
        .find_map(|line| line.strip_prefix("Hash:     "))
        .unwrap();
    assert!(hash.starts_with(row[2]), "list and show give the same hash");
    assert!(shown.contains("  urdf/arm7.urdf\n"), "{shown}");
}

#[test]
fn an_unknown_package_and_a_broken_one_are_reported_with_their_cause() {
    let packages = Packages::new("broken");
    packages.add_arm7("arm7");
    let broken = packages.add_arm7("broken");
    std::fs::remove_file(broken.join("urdf/arm7.urdf")).unwrap();

    let output = kapto(&packages.0, &["package", "list"]);
    assert!(!output.status.success());
    assert!(
        text(&output.stdout).contains("arm7"),
        "the good one is still listed"
    );
    assert!(
        text(&output.stderr).starts_with("error[package.missing_file]"),
        "{}",
        text(&output.stderr)
    );

    let output = kapto(&packages.0, &["package", "show", "nope"]);
    assert!(!output.status.success());
    assert!(text(&output.stderr).starts_with("error[package.not_found]"));

    let empty = Packages::new("empty");
    let output = kapto(&empty.0, &["package", "list"]);
    assert!(output.status.success());
    assert!(text(&output.stdout).starts_with("No robot packages in "));
}

#[test]
fn lint_fails_on_errors_but_not_on_warnings_and_reports_json_for_tools() {
    let packages = Packages::new("lint");
    let folder = packages.add_arm7("arm7");
    let clean = kapto(&packages.0, &["package", "lint", "arm7"]);
    assert!(clean.status.success());
    assert!(text(&clean.stdout).ends_with(": 0 errors, 0 warnings\n"));

    let mapping = folder.join("mapping.toml");
    let text_before = std::fs::read_to_string(&mapping).unwrap();
    // A warning only: joint_7 isn't mapped.
    std::fs::write(
        &mapping,
        text_before.replace("joint_7 = \"arm_joints.joint_positions[6]\"\n", ""),
    )
    .unwrap();
    let warned = kapto(&packages.0, &["package", "lint", "arm7"]);
    assert!(warned.status.success(), "{}", text(&warned.stdout));
    assert!(text(&warned.stdout).contains("warning[mapping.joint_unmapped]"));

    // An error: a joint the robot doesn't have.
    std::fs::write(
        &mapping,
        text_before.replace("[joints]\n", "[joints]\nelbow = \"arm_joints.gripper\"\n"),
    )
    .unwrap();
    let failed = kapto(
        &packages.0,
        &["package", "lint", "arm7", "--format", "json"],
    );
    assert!(!failed.status.success());
    let report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["errors"], 1);
    assert_eq!(report["findings"][0]["code"], "mapping.unknown_joint");
    assert_eq!(report["findings"][0]["severity"], "error");
}

#[test]
fn a_urdf_from_elsewhere_is_imported_as_a_package_that_lints_and_isnt_imported_twice() {
    let packages = Packages::new("import");
    let elsewhere = packages.0.with_extension("elsewhere");
    let _ = std::fs::remove_dir_all(&elsewhere);
    let arm7 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7/urdf/arm7.urdf");
    std::fs::create_dir_all(elsewhere.join("urdf")).unwrap();
    std::fs::copy(&arm7, elsewhere.join("urdf/arm7.urdf")).unwrap();
    let urdf = elsewhere.join("urdf/arm7.urdf");
    let urdf = urdf.to_str().unwrap();

    let imported = kapto(
        &packages.0,
        &["package", "import", urdf, "--id", "sample_arm"],
    );
    assert!(imported.status.success(), "{}", text(&imported.stderr));
    let said = text(&imported.stdout);
    assert!(said.contains("as the package `sample_arm`"), "{said}");
    assert!(
        said.contains("Next: declare what the robot sends"),
        "{said}"
    );
    // A package Kapto finds, whose only warnings are the joints left to map.
    let listed = kapto(&packages.0, &["package", "list"]);
    assert!(text(&listed.stdout).contains("sample_arm"));
    let checked = kapto(&packages.0, &["package", "lint", "sample_arm"]);
    assert!(checked.status.success(), "{}", text(&checked.stdout));
    assert!(text(&checked.stdout).contains("warning[mapping.joint_unmapped]"));

    let again = kapto(
        &packages.0,
        &["package", "import", urdf, "--id", "sample_arm"],
    );
    assert!(!again.status.success());
    assert!(
        text(&again.stderr).contains("error[package.import_exists]"),
        "{}",
        text(&again.stderr)
    );
    let _ = std::fs::remove_dir_all(&elsewhere);
}
