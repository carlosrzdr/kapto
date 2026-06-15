// SPDX-License-Identifier: Apache-2.0

//! The package linter: the sample arm is clean, and each rule finds the one
//! thing broken in a copy of it made to break that rule.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_config::QaSettings;
use kapto_core::ErrorCode;
use kapto_robot::{LintReport, RULES, Severity, lint};

/// A copy of the sample arm for one test, removed when dropped.
struct Fixture(PathBuf);

impl Fixture {
    fn new(test: &str) -> Self {
        let folder = std::env::temp_dir().join(format!("kapto-lint-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let arm7 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7");
        for file in [
            "package.toml",
            "streams.toml",
            "mapping.toml",
            "qa.toml",
            "urdf/arm7.urdf",
        ] {
            let to = folder.join(file);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(arm7.join(file), to).unwrap();
        }
        Self(folder)
    }

    /// Replaces `from` with `to` in `file`, which must contain it.
    fn edit(&self, file: &str, from: &str, to: &str) -> &Self {
        let path = self.0.join(file);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{file} has no {from:?}");
        std::fs::write(path, text.replacen(from, to, 1)).unwrap();
        self
    }

    fn append(&self, file: &str, text: &str) -> &Self {
        let path = self.0.join(file);
        let mut contents = std::fs::read_to_string(&path).unwrap();
        contents.push_str(text);
        std::fs::write(path, contents).unwrap();
        self
    }

    fn write(&self, file: &str, contents: &[u8]) -> &Self {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        self
    }

    /// Gives the base link a visual drawn from `mesh`, relative to the URDF.
    fn mesh(&self, mesh: &str) -> &Self {
        self.edit(
            "urdf/arm7.urdf",
            "<link name=\"base_link\">",
            &format!(
                "<link name=\"base_link\">\n    <visual><geometry><mesh filename=\"{mesh}\"/></geometry></visual>"
            ),
        )
    }

    fn lint(&self) -> LintReport {
        lint(&self.0, &[])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Checks that the report has exactly one finding, of `code`, at `file`
/// and `line` if given, with the severity its rule says.
fn only(report: &LintReport, code: ErrorCode, file: Option<&str>, line: Option<usize>) {
    let codes: Vec<&str> = report.findings.iter().map(|finding| finding.code).collect();
    assert_eq!(codes, [code.id()], "{}", report.to_text());
    let finding = &report.findings[0];
    let rule = RULES.iter().find(|rule| rule.code == code).unwrap();
    assert_eq!(finding.severity, rule.severity, "{}", code.id());
    assert_eq!(finding.remedy, code.remedy());
    if file.is_some() {
        assert_eq!(finding.file.as_deref(), file, "{}", report.to_text());
    }
    if line.is_some() {
        assert_eq!(finding.line, line, "{}", report.to_text());
    }
    let (errors, warnings) = match rule.severity {
        Severity::Error => (1, 0),
        Severity::Warning => (0, 1),
    };
    assert_eq!((report.errors, report.warnings), (errors, warnings));
}

/// The codes the tests below break the arm for, one test each.
const TESTED: [ErrorCode; 23] = [
    ErrorCode::PackageNotFound,
    ErrorCode::PackageUnknownKey,
    ErrorCode::ConfigInvalidSyntax,
    ErrorCode::ConfigInvalidValue,
    ErrorCode::PackageMissingFile,
    ErrorCode::PackageUnsafePath,
    ErrorCode::RobotUrdfInvalid,
    ErrorCode::RobotKinematicTreeInvalid,
    ErrorCode::RobotJointRangeEmpty,
    ErrorCode::RobotJointLimitsUnknown,
    ErrorCode::RobotMeshUnreadable,
    ErrorCode::RobotMeshUnsupportedFormat,
    ErrorCode::RobotMeshInvalid,
    ErrorCode::RobotMeshYUp,
    ErrorCode::StreamUnknownKey,
    ErrorCode::DecoderInvalidLayout,
    ErrorCode::MappingUnknownKey,
    ErrorCode::QaUnknownKey,
    ErrorCode::LayoutUnknownKey,
    ErrorCode::MappingUnknownJoint,
    ErrorCode::MappingUnknownField,
    ErrorCode::MappingUnitMismatch,
    ErrorCode::MappingJointUnmapped,
];

#[test]
fn every_rule_has_a_broken_fixture() {
    let rules: Vec<ErrorCode> = RULES.iter().map(|rule| rule.code).collect();
    assert_eq!(rules, TESTED);
}

#[test]
fn the_sample_arm_is_clean() {
    let report = Fixture::new("clean").lint();
    assert_eq!(report.findings, [], "{}", report.to_text());
}

#[test]
fn a_folder_without_package_toml() {
    let fixture = Fixture::new("not-found");
    std::fs::remove_file(fixture.0.join("package.toml")).unwrap();
    only(&fixture.lint(), ErrorCode::PackageNotFound, None, None);
}

#[test]
fn an_unknown_key_in_package_toml() {
    let fixture = Fixture::new("package-key");
    fixture.append("package.toml", "vendr = \"Acme\"\n");
    only(
        &fixture.lint(),
        ErrorCode::PackageUnknownKey,
        Some("package.toml"),
        Some(12),
    );
}

#[test]
fn invalid_toml() {
    let fixture = Fixture::new("syntax");
    fixture.append("streams.toml", "oops =\n");
    only(
        &fixture.lint(),
        ErrorCode::ConfigInvalidSyntax,
        Some("streams.toml"),
        None,
    );
}

#[test]
fn an_invalid_value() {
    let fixture = Fixture::new("value");
    fixture.edit("package.toml", "version = \"0.1.0\"", "version = \"0 1\"");
    only(
        &fixture.lint(),
        ErrorCode::ConfigInvalidValue,
        Some("package.toml"),
        Some(4),
    );
}

#[test]
fn a_missing_file() {
    let fixture = Fixture::new("missing");
    std::fs::remove_file(fixture.0.join("mapping.toml")).unwrap();
    only(&fixture.lint(), ErrorCode::PackageMissingFile, None, None);
}

#[test]
fn a_symbolic_link() {
    let fixture = Fixture::new("symlink");
    std::os::unix::fs::symlink("/etc/hostname", fixture.0.join("notes.txt")).unwrap();
    only(
        &fixture.lint(),
        ErrorCode::PackageUnsafePath,
        Some("notes.txt"),
        None,
    );
}

#[test]
fn a_moving_joint_without_limits() {
    let fixture = Fixture::new("urdf");
    fixture.edit(
        "urdf/arm7.urdf",
        "<limit lower=\"-2.9\" upper=\"2.9\" effort=\"100\" velocity=\"2\"/>",
        "",
    );
    only(&fixture.lint(), ErrorCode::RobotUrdfInvalid, None, None);
}

#[test]
fn a_link_not_joined_to_the_rest() {
    let fixture = Fixture::new("tree");
    fixture.edit(
        "urdf/arm7.urdf",
        "</robot>",
        "  <link name=\"loose\"/>\n</robot>",
    );
    only(
        &fixture.lint(),
        ErrorCode::RobotKinematicTreeInvalid,
        None,
        None,
    );
}

#[test]
fn a_joint_whose_limits_leave_no_room() {
    let fixture = Fixture::new("range");
    fixture.edit(
        "urdf/arm7.urdf",
        "<limit lower=\"-2.9\" upper=\"2.9\"",
        "<limit lower=\"2.9\" upper=\"2.9\"",
    );
    let report = fixture.lint();
    only(&report, ErrorCode::RobotJointRangeEmpty, None, None);
    assert_eq!(
        report.findings[0].context,
        [("joint".to_owned(), "joint_1".to_owned())]
    );
}

#[test]
fn a_joint_without_an_effort_limit() {
    let fixture = Fixture::new("effort");
    fixture.edit("urdf/arm7.urdf", "effort=\"100\"", "effort=\"0\"");
    only(
        &fixture.lint(),
        ErrorCode::RobotJointLimitsUnknown,
        None,
        None,
    );
}

#[test]
fn a_mesh_that_isnt_there() {
    let fixture = Fixture::new("mesh-missing");
    fixture.mesh("../meshes/base.stl");
    only(&fixture.lint(), ErrorCode::RobotMeshUnreadable, None, None);
}

#[test]
fn a_mesh_in_another_format() {
    let fixture = Fixture::new("mesh-format");
    fixture
        .mesh("../meshes/base.ply")
        .write("meshes/base.ply", b"ply\n");
    only(
        &fixture.lint(),
        ErrorCode::RobotMeshUnsupportedFormat,
        Some("meshes/base.ply"),
        None,
    );
}

#[test]
fn a_mesh_that_isnt_valid() {
    let fixture = Fixture::new("mesh-invalid");
    fixture
        .mesh("../meshes/base.stl")
        .write("meshes/base.stl", b"not a mesh");
    only(
        &fixture.lint(),
        ErrorCode::RobotMeshInvalid,
        Some("meshes/base.stl"),
        None,
    );
}

#[test]
fn a_mesh_made_with_y_up() {
    let fixture = Fixture::new("mesh-y-up");
    let dae = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/meshes/handmade/millimetre_triangle.dae");
    fixture
        .mesh("../meshes/base.dae")
        .write("meshes/base.dae", &std::fs::read(dae).unwrap());
    only(&fixture.lint(), ErrorCode::RobotMeshYUp, None, None);
}

#[test]
fn an_unknown_key_in_streams_toml() {
    let fixture = Fixture::new("stream-key");
    fixture.edit("streams.toml", "rate_hz = 2000", "rate_hz = 2000\nrate = 5");
    only(
        &fixture.lint(),
        ErrorCode::StreamUnknownKey,
        Some("streams.toml"),
        Some(6),
    );
}

#[test]
fn an_invalid_decoder_field() {
    let fixture = Fixture::new("layout");
    fixture.edit("streams.toml", "name = \"sequence\"", "name = \"Sequence\"");
    only(
        &fixture.lint(),
        ErrorCode::DecoderInvalidLayout,
        Some("streams.toml"),
        Some(14),
    );
}

#[test]
fn an_unknown_key_in_mapping_toml() {
    let fixture = Fixture::new("mapping-key");
    fixture.append("mapping.toml", "[frames]\n");
    only(
        &fixture.lint(),
        ErrorCode::MappingUnknownKey,
        Some("mapping.toml"),
        Some(18),
    );
}

#[test]
fn a_mapped_joint_the_robot_doesnt_have() {
    let fixture = Fixture::new("joint");
    fixture.edit(
        "mapping.toml",
        "[joints]\n",
        "[joints]\nelbow = \"arm_joints.gripper\"\n",
    );
    only(&fixture.lint(), ErrorCode::MappingUnknownJoint, None, None);
}

#[test]
fn a_mapped_field_the_stream_doesnt_decode() {
    let fixture = Fixture::new("field");
    fixture.edit("mapping.toml", "joint_positions[0]", "joint_angles[0]");
    only(&fixture.lint(), ErrorCode::MappingUnknownField, None, None);
}

#[test]
fn a_mapped_field_in_the_wrong_unit() {
    let fixture = Fixture::new("unit");
    fixture.edit(
        "mapping.toml",
        "gripper = \"arm_joints.gripper\"",
        "gripper = \"arm_joints.joint_positions[6]\"",
    );
    only(&fixture.lint(), ErrorCode::MappingUnitMismatch, None, None);
}

#[test]
fn a_joint_without_a_mapping() {
    let fixture = Fixture::new("unmapped");
    fixture.edit(
        "mapping.toml",
        "joint_7 = \"arm_joints.joint_positions[6]\"\n",
        "",
    );
    only(&fixture.lint(), ErrorCode::MappingJointUnmapped, None, None);
}

/// A package with one error and one warning, rendered both ways.
fn mixed(test: &str) -> (Fixture, LintReport) {
    let fixture = Fixture::new(test);
    fixture
        .edit("urdf/arm7.urdf", "effort=\"100\"", "effort=\"0\"")
        .append("streams.toml", "[streams.arm_joints.extra]\n");
    let report = fixture.lint();
    (fixture, report)
}

#[test]
fn the_text_report_lists_errors_first_with_where_and_what_to_do() {
    let (fixture, report) = mixed("text");
    let folder = fixture.0.display().to_string();
    assert_eq!(
        report.to_text(),
        format!(
            "{folder}: 1 error, 1 warning\n\
             \n\
             error[stream.unknown_key] streams.toml:38: Unknown key in a stream declaration: `streams.arm_joints.extra`\n\
             \x20 remedy: {}\n\
             \n\
             warning[robot.joint_limits_unknown]: A joint has no velocity or effort limit: joint `joint_1` has no effort limit\n\
             \x20 joint: joint_1\n\
             \x20 remedy: {}\n",
            ErrorCode::StreamUnknownKey.remedy(),
            ErrorCode::RobotJointLimitsUnknown.remedy()
        )
    );
}

#[test]
fn the_json_report_has_the_same_findings_for_tools() {
    let (fixture, report) = mixed("json");
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "package": fixture.0.display().to_string(),
            "errors": 1,
            "warnings": 1,
            "findings": [
                {
                    "code": "stream.unknown_key",
                    "severity": "error",
                    "message": "Unknown key in a stream declaration: `streams.arm_joints.extra`",
                    "remedy": ErrorCode::StreamUnknownKey.remedy(),
                    "file": "streams.toml",
                    "line": 38
                },
                {
                    "code": "robot.joint_limits_unknown",
                    "severity": "warning",
                    "message": "A joint has no velocity or effort limit: joint `joint_1` has no effort limit",
                    "remedy": ErrorCode::RobotJointLimitsUnknown.remedy(),
                    "context": [["joint", "joint_1"]]
                }
            ]
        })
    );
}

#[test]
fn an_unknown_setting_in_qa_toml() {
    let fixture = Fixture::new("qa-key");
    fixture.edit("qa.toml", "max_dropped = 0", "max_droped = 0");
    only(
        &fixture.lint(),
        ErrorCode::QaUnknownKey,
        Some("qa.toml"),
        Some(10),
    );
}

#[test]
fn a_mistake_in_the_layout_is_found_at_its_line() {
    let fixture = Fixture::new("layout-mistake");
    fixture.write(
        "layout.toml",
        b"[[columns]]\npanels = [\n  { kind = \"health\" },\n  { kind = \"robot\", heigth = 2 },\n]\n",
    );
    only(
        &fixture.lint(),
        ErrorCode::LayoutUnknownKey,
        Some("layout.toml"),
        Some(4),
    );
}

#[test]
fn a_layout_the_package_names_is_read_from_where_it_says() {
    let fixture = Fixture::new("own-layout-named");
    fixture
        .append("package.toml", "layout = \"views/arm.toml\"\n")
        .write(
            "views/arm.toml",
            b"[[columns]]\npanels = [{ kind = \"health\" }]\n",
        );
    assert_eq!(fixture.lint().findings, []);
    let package = kapto_robot::load_package(&fixture.0).unwrap();
    let layout = package.own_layout().unwrap().unwrap();
    assert_eq!(
        layout.columns[0].panels[0].kind,
        kapto_config::PanelKind::Health
    );
}

#[test]
fn a_package_without_qa_toml_runs_every_check_with_its_defaults() {
    let fixture = Fixture::new("no-qa");
    std::fs::remove_file(fixture.0.join("qa.toml")).unwrap();
    assert_eq!(fixture.lint().findings, []);
    let package = kapto_robot::load_package(&fixture.0).unwrap();
    assert_eq!(package.qa_settings().unwrap(), QaSettings::default());
}
