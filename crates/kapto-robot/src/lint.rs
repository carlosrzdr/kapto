// SPDX-License-Identifier: Apache-2.0

//! The package linter: everything wrong with a robot package, found in one
//! pass and reported with where and what to do.
//!
//! The package is loaded the way Kapto loads it to run: `package.toml`, the
//! URDF, each mesh, `streams.toml`, then `mapping.toml` against the robot
//! and the streams, and `qa.toml` if it has one. Every problem a step finds is a finding; a step that
//! fails stops only the steps that need its result. Whether a finding is an
//! error (Kapto can't use the package) or a warning (it can, with less) is
//! data, in [`RULES`].

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use kapto_config::{ConfigFile, parse_mapping, parse_streams, read_config_file};
use kapto_core::{ErrorCode, KaptoError};
use serde::Serialize;

use crate::kinematics::Kinematics;
use crate::mapping::JointMap;
use crate::mesh::{UpAxis, load_mesh};
use crate::model::{Geometry, JointKind, RobotModel};
use crate::package::{Package, load_package};
use crate::urdf::load_urdf;

/// How much a finding matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Kapto can't use the package as it is.
    Error,
    /// Kapto can use the package, but something won't work or will be
    /// missing.
    Warning,
}

/// One rule: a cause code, how much it matters, and what it checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The cause code of its findings.
    pub code: ErrorCode,
    /// How much its findings matter.
    pub severity: Severity,
    /// What it checks, for documentation.
    pub checks: &'static str,
}

/// Every rule, in the order the linter applies them. A problem whose code
/// isn't listed is an error.
pub const RULES: &[Rule] = &[
    Rule {
        code: ErrorCode::PackageNotFound,
        severity: Severity::Error,
        checks: "The folder has a `package.toml`.",
    },
    Rule {
        code: ErrorCode::PackageUnknownKey,
        severity: Severity::Error,
        checks: "`package.toml` has only keys Kapto knows.",
    },
    Rule {
        code: ErrorCode::ConfigInvalidSyntax,
        severity: Severity::Error,
        checks: "`package.toml`, `streams.toml`, `mapping.toml` and `qa.toml` are valid TOML.",
    },
    Rule {
        code: ErrorCode::ConfigInvalidValue,
        severity: Severity::Error,
        checks: "Every value in them has the right type and range, and every required one is there.",
    },
    Rule {
        code: ErrorCode::PackageMissingFile,
        severity: Severity::Error,
        checks: "The URDF, `streams.toml` and `mapping.toml` that `package.toml` names are there.",
    },
    Rule {
        code: ErrorCode::PackageUnsafePath,
        severity: Severity::Error,
        checks: "The package is plain files and folders with UTF-8 names, without symbolic links.",
    },
    Rule {
        code: ErrorCode::RobotUrdfInvalid,
        severity: Severity::Error,
        checks: "The URDF is valid: every link and joint well formed, every moving joint with a `<limit>`.",
    },
    Rule {
        code: ErrorCode::RobotKinematicTreeInvalid,
        severity: Severity::Error,
        checks: "The joints form one tree from a single root link.",
    },
    Rule {
        code: ErrorCode::RobotJointRangeEmpty,
        severity: Severity::Warning,
        checks: "Each joint that moves within limits has a lower limit below its upper one.",
    },
    Rule {
        code: ErrorCode::RobotJointLimitsUnknown,
        severity: Severity::Warning,
        checks: "Each joint that moves within limits has a velocity and an effort limit.",
    },
    Rule {
        code: ErrorCode::RobotMeshUnreadable,
        severity: Severity::Warning,
        checks: "Every mesh and texture the URDF refers to is there and readable; without it, its part isn't shown.",
    },
    Rule {
        code: ErrorCode::RobotMeshUnsupportedFormat,
        severity: Severity::Warning,
        checks: "Every mesh is STL, OBJ, COLLADA or glTF.",
    },
    Rule {
        code: ErrorCode::RobotMeshInvalid,
        severity: Severity::Warning,
        checks: "Every mesh file is valid in its format.",
    },
    Rule {
        code: ErrorCode::RobotMeshYUp,
        severity: Severity::Warning,
        checks: "No mesh says it was made with Y up.",
    },
    Rule {
        code: ErrorCode::StreamUnknownKey,
        severity: Severity::Error,
        checks: "`streams.toml` has only keys Kapto knows.",
    },
    Rule {
        code: ErrorCode::DecoderInvalidLayout,
        severity: Severity::Error,
        checks: "Every stream's decoder fields are valid.",
    },
    Rule {
        code: ErrorCode::MappingUnknownKey,
        severity: Severity::Error,
        checks: "`mapping.toml` has only keys Kapto knows.",
    },
    Rule {
        code: ErrorCode::QaUnknownKey,
        severity: Severity::Error,
        checks: "`qa.toml` has only the quality checks' settings, each in the table named after its check.",
    },
    Rule {
        code: ErrorCode::LayoutUnknownKey,
        severity: Severity::Error,
        checks: "The layout, if the package has one, has only columns of panels and their keys.",
    },
    Rule {
        code: ErrorCode::MappingUnknownJoint,
        severity: Severity::Error,
        checks: "Every joint `mapping.toml` maps is a moving joint of the robot.",
    },
    Rule {
        code: ErrorCode::MappingUnknownField,
        severity: Severity::Error,
        checks: "Every field `mapping.toml` names is decoded by its stream, and is a number.",
    },
    Rule {
        code: ErrorCode::MappingUnitMismatch,
        severity: Severity::Error,
        checks: "Every mapped field's unit fits its joint: radians for turning joints, metres for sliding ones.",
    },
    Rule {
        code: ErrorCode::MappingJointUnmapped,
        severity: Severity::Warning,
        checks: "Every moving joint has a position mapped; without one it stays at 0 in the live view.",
    },
];

/// One problem the linter found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Its cause code, e.g. `mapping.unknown_field`.
    pub code: &'static str,
    /// How much it matters.
    pub severity: Severity,
    /// What is wrong.
    pub message: String,
    /// What to do about it.
    pub remedy: &'static str,
    /// The file it is in, relative to the package folder where it is inside
    /// it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The line in that file, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// Anything else that says where, e.g. the joint or link.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<(String, String)>,
}

/// What the linter found in one package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LintReport {
    /// The package folder.
    pub package: String,
    /// Errors found.
    pub errors: usize,
    /// Warnings found.
    pub warnings: usize,
    /// Every finding, errors first, otherwise in the order found.
    pub findings: Vec<Finding>,
}

/// Lints the package in `folder`. Mesh and texture files are also looked
/// for in `search_paths`, as when the robot is loaded to run.
#[must_use]
pub fn lint(folder: &Path, search_paths: &[PathBuf]) -> LintReport {
    let mut found = Found {
        folder,
        findings: Vec::new(),
    };
    match load_package(folder) {
        Ok(package) => lint_contents(&package, search_paths, &mut found),
        Err(errors) => found.all(&errors),
    }
    found.report()
}

/// Everything after `package.toml`: the robot, its streams and mapping,
/// and the quality checks' settings.
fn lint_contents(package: &Package, search_paths: &[PathBuf], found: &mut Found<'_>) {
    let robot = load_urdf(&package.urdf(), search_paths).map_err(|errors| found.all(&errors));
    let kinematics = robot.as_ref().ok().and_then(|robot| {
        found.all(&joint_limits(robot));
        found.all(&meshes(robot));
        Kinematics::new(robot)
            .map_err(|error| found.one(&error))
            .ok()
    });
    let streams = read(&package.streams())
        .and_then(|file| parse_streams(&file))
        .map_err(|errors| found.all(&errors));
    let mapping_path = package.mapping();
    let mapping = read(&mapping_path)
        .and_then(|file| parse_mapping(&file))
        .map_err(|errors| found.all(&errors));
    if let (Ok(robot), Some(kinematics), Ok(streams), Ok(mapping)) =
        (&robot, &kinematics, &streams, &mapping)
    {
        match JointMap::new(robot, kinematics, streams, mapping, &mapping_path) {
            Ok(map) => found.all(map.warnings()),
            Err(errors) => found.all(&errors),
        }
    }
    if let Err(errors) = package.qa_settings() {
        found.all(&errors);
    }
    if let Err(errors) = package.own_layout() {
        found.all(&errors);
    }
}

/// The findings so far.
struct Found<'a> {
    folder: &'a Path,
    findings: Vec<Finding>,
}

impl Found<'_> {
    fn one(&mut self, problem: &KaptoError) {
        self.findings.push(finding(self.folder, problem));
    }

    fn all(&mut self, problems: &[KaptoError]) {
        for problem in problems {
            self.one(problem);
        }
    }

    fn report(mut self) -> LintReport {
        // Stable: within a severity, the order found.
        self.findings.sort_by_key(|finding| finding.severity);
        let errors = self
            .findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .count();
        LintReport {
            package: self.folder.display().to_string(),
            errors,
            warnings: self.findings.len() - errors,
            findings: self.findings,
        }
    }
}

fn read(path: &Path) -> Result<ConfigFile, Vec<KaptoError>> {
    match read_config_file(path, true) {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(vec![
            KaptoError::new(ErrorCode::PackageMissingFile).detail(path.display().to_string()),
        ]),
        Err(error) => Err(vec![error]),
    }
}

/// Limits that say nothing: an empty range, or no velocity or effort.
fn joint_limits(robot: &RobotModel) -> Vec<KaptoError> {
    let mut problems = Vec::new();
    for joint in &robot.joints {
        let (Some(limits), JointKind::Revolute | JointKind::Prismatic) =
            (&joint.limits, joint.kind)
        else {
            continue;
        };
        if limits.lower >= limits.upper {
            problems.push(
                KaptoError::new(ErrorCode::RobotJointRangeEmpty)
                    .detail(format!(
                        "joint `{}` goes from {} to {}",
                        joint.name, limits.lower, limits.upper
                    ))
                    .context("joint", joint.name.clone()),
            );
        }
        let unknown: Vec<&str> = [("velocity", limits.velocity), ("effort", limits.effort)]
            .into_iter()
            .filter(|(_, limit)| *limit <= 0.0)
            .map(|(name, _)| name)
            .collect();
        if !unknown.is_empty() {
            problems.push(
                KaptoError::new(ErrorCode::RobotJointLimitsUnknown)
                    .detail(format!(
                        "joint `{}` has no {} limit",
                        joint.name,
                        unknown.join(" or ")
                    ))
                    .context("joint", joint.name.clone()),
            );
        }
    }
    problems
}

/// Every mesh file referred to, missing, unreadable or made with Y up;
/// each file once.
fn meshes(robot: &RobotModel) -> Vec<KaptoError> {
    let mut problems = Vec::new();
    for (link, resource) in robot.missing_files() {
        problems.push(
            KaptoError::new(ErrorCode::RobotMeshUnreadable)
                .detail(format!("{} isn't there", resource.uri))
                .context("link", link.name.clone()),
        );
    }
    let mut seen = BTreeSet::new();
    for link in &robot.links {
        for shape in link.visuals.iter().chain(&link.collisions) {
            let Geometry::Mesh { file, .. } = &shape.geometry else {
                continue;
            };
            let Some(path) = &file.path else {
                continue;
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            match load_mesh(path) {
                Ok(mesh) if mesh.up_axis == Some(UpAxis::Y) => problems.push(
                    KaptoError::new(ErrorCode::RobotMeshYUp)
                        .detail(file.uri.clone())
                        .context("link", link.name.clone()),
                ),
                Ok(_) => {}
                Err(error) => problems.push(error.context("link", link.name.clone())),
            }
        }
    }
    problems
}

/// The severity of findings with `code`.
fn severity(code: ErrorCode) -> Severity {
    RULES
        .iter()
        .find(|rule| rule.code == code)
        .map_or(Severity::Error, |rule| rule.severity)
}

fn finding(folder: &Path, problem: &KaptoError) -> Finding {
    let mut file = None;
    let mut line = None;
    let mut context = Vec::new();
    for (key, value) in problem.context_entries() {
        match *key {
            "file" | "path" if file.is_none() => {
                let path = lexical(Path::new(value));
                let inside = path.strip_prefix(lexical(folder)).unwrap_or(&path);
                file = Some(inside.display().to_string());
            }
            "line" => line = value.parse().ok(),
            _ => context.push(((*key).to_owned(), value.clone())),
        }
    }
    Finding {
        code: problem.code().id(),
        severity: severity(problem.code()),
        message: problem.message(),
        remedy: problem.remedy(),
        file,
        line,
        context,
    }
}

/// `path` with `.` and `folder/..` taken out, without asking the file
/// system: `urdf/../meshes/a.stl` is `meshes/a.stl`.
fn lexical(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(clean.components().next_back(), Some(Component::Normal(_))) =>
            {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    clean
}

impl LintReport {
    /// The report for people: a summary line, then each finding with where
    /// it is and what to do.
    #[must_use]
    pub fn to_text(&self) -> String {
        let plural = |count: usize, word: &str| {
            if count == 1 {
                format!("1 {word}")
            } else {
                format!("{count} {word}s")
            }
        };
        let mut text = format!(
            "{}: {}, {}\n",
            self.package,
            plural(self.errors, "error"),
            plural(self.warnings, "warning")
        );
        for finding in &self.findings {
            let severity = match finding.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            let place = match (&finding.file, finding.line) {
                (Some(file), Some(line)) => format!(" {file}:{line}"),
                (Some(file), None) => format!(" {file}"),
                _ => String::new(),
            };
            let _ = writeln!(
                text,
                "\n{severity}[{}]{place}: {}",
                finding.code, finding.message
            );
            for (key, value) in &finding.context {
                let _ = writeln!(text, "  {key}: {value}");
            }
            let _ = writeln!(text, "  remedy: {}", finding.remedy);
        }
        text
    }

    /// The report as JSON, for tools and the web UI.
    #[must_use]
    pub fn to_json(&self) -> String {
        // Plain strings and numbers always serialise.
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}
