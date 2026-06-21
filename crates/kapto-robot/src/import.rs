// SPDX-License-Identifier: Apache-2.0

//! Making a robot package from a robot description found elsewhere, such
//! as a vendor's repository: the URDF as it is, with every mesh and texture
//! it names, laid out so each reference finds its file inside the package
//! as written, and the `package.toml`, `streams.toml` and `mapping.toml`
//! that make it a package.
//!
//! A file named by `package://<name>/<rest>` goes to `<name>/<rest>` in the
//! package, which Kapto looks for beside the folders above the URDF. Files
//! named by relative paths keep their place relative to the URDF: the URDF
//! and they are copied from the deepest folder that holds them all. A file
//! named by an absolute path can't be carried, and one that isn't found
//! can't be copied; both are said, and the rest is imported.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use kapto_config::PACKAGE_FILE;
use kapto_core::name::is_name;
use kapto_core::{ErrorCode, KaptoError};

use crate::model::{JointKind, RobotModel};

use crate::urdf::{load_urdf, parse_urdf};
use crate::xacro::expand_xacro;

/// A package made by [`import_package`].
#[derive(Debug)]
pub struct Imported {
    /// The package's folder.
    pub folder: PathBuf,
    /// The robot description's path in the package.
    pub urdf: PathBuf,
    /// The robot's moving joints, which `mapping.toml` lists to be mapped.
    pub joints: Vec<String>,
    /// Each file copied besides the URDF: its path in the package and its
    /// size in bytes.
    pub copied: Vec<(PathBuf, u64)>,
    /// What the URDF names that wasn't copied, and why.
    pub left_out: Vec<KaptoError>,
}

/// Makes the package `id` in `folder` from the robot description at
/// `urdf`, whose `package://` files are looked for in `search_paths` as
/// Kapto does when it runs. A description in xacro (`.xacro`) is expanded
/// with `args` first, and the package holds the URDF it expands to. `name`
/// is what people call the robot; the URDF's robot name if not given.
///
/// # Errors
///
/// `config.invalid_value` if `id` isn't an ID or `args` are given for a
/// URDF, `package.import_exists` if `folder` exists, `robot.xacro_failed`
/// if a xacro description doesn't expand, the URDF's own errors if it can't
/// be read, and `package.unreadable` if a file can't be copied.
pub fn import_package(
    urdf: &Path,
    args: &BTreeMap<String, String>,
    search_paths: &[PathBuf],
    folder: &Path,
    id: &str,
    name: Option<&str>,
) -> Result<Imported, Vec<KaptoError>> {
    if !is_name(id) {
        return Err(vec![KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
            "the package ID is {}: an ID such as \"arm7\", of lowercase letters, digits and `_`, starting with a letter",
            quoted(id)
        ))]);
    }
    if folder.exists() {
        return Err(vec![
            KaptoError::new(ErrorCode::PackageImportExists).detail(folder.display().to_string()),
        ]);
    }
    let xacro = urdf
        .extension()
        .is_some_and(|extension| extension == "xacro");
    if !xacro && !args.is_empty() {
        return Err(vec![KaptoError::new(ErrorCode::ConfigInvalidValue).detail(
            format!(
                "arguments are given, but {} is URDF: only a xacro description takes them",
                urdf.display()
            ),
        )]);
    }
    // A xacro description is read as the URDF it expands to, as if that
    // were beside it: its files are found from the same folder.
    let (model, expanded, as_urdf) = if xacro {
        let text = expand_xacro(urdf, args, search_paths).map_err(|error| vec![error])?;
        let as_urdf = urdf.with_extension("");
        let as_urdf = if as_urdf
            .extension()
            .is_some_and(|extension| extension == "urdf")
        {
            as_urdf
        } else {
            as_urdf.with_extension("urdf")
        };
        (
            parse_urdf(&text, &as_urdf, search_paths)?,
            Some(text),
            as_urdf,
        )
    } else {
        (load_urdf(urdf, search_paths)?, None, urdf.to_path_buf())
    };
    let Placed {
        urdf_place,
        places,
        left_out,
    } = place(&model, &as_urdf);
    let write_error = |path: &Path, error: std::io::Error| {
        vec![
            KaptoError::new(ErrorCode::PackageUnreadable)
                .detail(format!("can't write {}: {error}", path.display())),
        ]
    };
    let copy = |from: &Path, place: &Path| -> Result<u64, Vec<KaptoError>> {
        let to = folder.join(place);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|error| write_error(parent, error))?;
        }
        std::fs::copy(from, &to).map_err(|error| write_error(&to, error))
    };
    match &expanded {
        Some(text) => {
            let to = folder.join(&urdf_place);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|error| write_error(parent, error))?;
            }
            std::fs::write(&to, text).map_err(|error| write_error(&to, error))?;
        }
        None => {
            copy(urdf, &urdf_place)?;
        }
    }
    let mut copied = Vec::new();
    for (place, found) in &places {
        if place == &urdf_place {
            continue;
        }
        copied.push((place.clone(), copy(found, place)?));
    }
    let joints = moving_joints(&model);
    let files = [
        (
            PACKAGE_FILE,
            manifest(id, name.unwrap_or(&model.name), urdf, args, &urdf_place),
        ),
        ("streams.toml", STREAMS_TEMPLATE.to_owned()),
        ("mapping.toml", mapping(&urdf_place, &joints)),
    ];
    for (file, text) in files {
        let path = folder.join(file);
        std::fs::write(&path, text).map_err(|error| write_error(&path, error))?;
    }
    Ok(Imported {
        folder: folder.to_path_buf(),
        urdf: urdf_place,
        joints,
        copied,
        left_out,
    })
}

/// Where the URDF and each file it names go in the package, read from
/// where; and what can't go, and why.
struct Placed {
    urdf_place: PathBuf,
    places: BTreeMap<PathBuf, PathBuf>,
    left_out: Vec<KaptoError>,
}

fn place(model: &RobotModel, urdf: &Path) -> Placed {
    let base = urdf.parent().unwrap_or(Path::new(""));
    let mut left_out = Vec::new();
    // Each file's place in the package, by the path it is read from.
    let mut by_package: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut relative: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen = Vec::new();
    for (link, resource) in model.files() {
        if seen.contains(&resource.uri) {
            continue;
        }
        seen.push(resource.uri.clone());
        let Some(found) = &resource.path else {
            left_out.push(
                KaptoError::new(ErrorCode::RobotMeshUnreadable)
                    .detail(format!(
                        "{} wasn't found, so it isn't in the package",
                        resource.uri
                    ))
                    .context("link", link.name.clone()),
            );
            continue;
        };
        let not_portable = || {
            KaptoError::new(ErrorCode::PackageImportNotPortable)
                .detail(resource.uri.clone())
                .context("link", link.name.clone())
        };
        if let Some(rest) = resource.uri.strip_prefix("package://") {
            match inside(Path::new(rest)) {
                Some(place) => {
                    by_package.insert(place, found.clone());
                }
                None => left_out.push(not_portable()),
            }
        } else if resource.uri.starts_with("file://") || Path::new(&resource.uri).is_absolute() {
            left_out.push(not_portable());
        } else {
            relative.push((normalised(&base.join(&resource.uri)), found.clone()));
        }
    }
    // The deepest folder that holds the URDF and every file it names by a
    // relative path: the package keeps their layout from there.
    let urdf_path = normalised(urdf);
    let mut root = urdf_path.parent().unwrap_or(Path::new("")).to_path_buf();
    for (path, _) in &relative {
        while !path.starts_with(&root) {
            if !root.pop() {
                break;
            }
        }
    }
    let in_package = |path: &Path| path.strip_prefix(&root).map(Path::to_path_buf).ok();
    let urdf_place = in_package(&urdf_path).unwrap_or_else(|| PathBuf::from("robot.urdf"));
    let mut places: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for (path, found) in relative {
        if let Some(place) = in_package(&path) {
            places.insert(place, found);
        }
    }
    for (place, found) in by_package {
        places.entry(place).or_insert(found);
    }
    Placed {
        urdf_place,
        places,
        left_out,
    }
}

/// `path` with `.` and `..` worked out without asking the filesystem.
fn normalised(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `path` if it stays inside the folder it is relative to.
fn inside(path: &Path) -> Option<PathBuf> {
    path.components()
        .all(|part| matches!(part, Component::Normal(_)))
        .then(|| path.to_path_buf())
}

/// The joints a value can drive: those that move and don't follow another.
fn moving_joints(model: &RobotModel) -> Vec<String> {
    model
        .joints
        .iter()
        .filter(|joint| {
            matches!(
                joint.kind,
                JointKind::Revolute | JointKind::Continuous | JointKind::Prismatic
            ) && joint.mimic.is_none()
        })
        .map(|joint| joint.name.clone())
        .collect()
}

/// A TOML basic string of `text`.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A path in a package as `package.toml` writes it: separated by `/`.
fn slashed(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn manifest(
    id: &str,
    name: &str,
    from: &Path,
    args: &BTreeMap<String, String>,
    urdf: &Path,
) -> String {
    let mut source = from
        .file_name()
        .map_or_else(String::new, |file| file.to_string_lossy().into_owned());
    for (key, value) in args {
        let _ = write!(source, " {key}:={value}");
    }
    format!(
        "# Made by `kapto package import` from {source}. Kapto identifies a\n\
         # version of it by its ID and version, and by a hash of all its files.\n\
         id = {}\nversion = \"0.1.0\"\nname = {}\n\n\
         # The robot description, as imported. What the robot sends and how it\n\
         # drives the joints are in streams.toml and mapping.toml.\n\
         urdf = {}\n",
        quoted(id),
        quoted(name),
        quoted(&slashed(urdf)),
    )
}

/// What `streams.toml` starts as: nothing declared yet, and where to read
/// how.
const STREAMS_TEMPLATE: &str = "\
# What the robot sends: one table per stream, saying how it arrives (udp,
# tcp or ros2) and how values are read out of it. Nothing is declared yet.
# The package reference (docs/reference/package.md) has every key; for a
# ROS 2 robot, `kapto topics --streams` writes the declarations of the
# topics it finds, e.g.
#
#   [streams.joint_states]
#   rate_hz = 500
#   ros2 = { topic = \"/joint_states\", type = \"sensor_msgs/msg/JointState\" }
#
#   [streams.joint_states.decoder]
#   kind = \"cdr\"
#
#   [[streams.joint_states.decoder.fields]]
#   name = \"shoulder_pan_joint\"
#   type = \"f64\"
#   path = \"position[name=shoulder_pan_joint]\"
#   unit = \"rad\"
";

fn mapping(urdf: &Path, joints: &[String]) -> String {
    let mut text = format!(
        "# Which decoded value drives each moving joint of {}: give each the\n\
         # field of streams.toml that holds its position, e.g.\n\
         #   shoulder_pan_joint = \"joint_states.shoulder_pan_joint\"\n\
         [joints]\n",
        slashed(urdf)
    );
    for joint in joints {
        let _ = writeln!(text, "# {} = \"\"", quoted(joint));
    }
    text
}
