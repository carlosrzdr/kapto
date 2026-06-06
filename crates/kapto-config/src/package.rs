// SPDX-License-Identifier: Apache-2.0

//! What a robot package is, from its `package.toml`.
//!
//! ```toml
//! id = "arm7"
//! version = "0.1.0"
//! name = "Sample seven-joint arm"
//! vendor = "Kapto"
//! urdf = "urdf/arm7.urdf"
//! ```
//!
//! This module only reads the file; `kapto-robot` loads the package around
//! it and checks that its files are there.

use std::path::{Component, Path, PathBuf};

use kapto_core::name::is_name;
use kapto_core::{ErrorCode, KaptoError};
use toml_edit::Document;

use crate::file::ConfigFile;
use crate::located::syntax_error;
use crate::reader::{FileKey, Reader};

/// The name of the file that makes a folder a robot package.
pub const PACKAGE_FILE: &str = "package.toml";

/// Every key of `package.toml`, in the order they are documented.
pub const PACKAGE_KEYS: [FileKey; 9] = [
    FileKey {
        name: "id",
        required: true,
        help: "The package's ID: lowercase letters, digits and underscores, starting with a letter, e.g. `arm7`. Unique among the packages on one machine.",
    },
    FileKey {
        name: "version",
        required: true,
        help: "The package's version as its author numbers them, e.g. `1.2.0`; text without spaces. With the content hash, it identifies a version of the package.",
    },
    FileKey {
        name: "name",
        required: false,
        help: "What people call the robot, e.g. `Sample seven-joint arm`.",
    },
    FileKey {
        name: "vendor",
        required: false,
        help: "Who makes the robot or wrote the package.",
    },
    FileKey {
        name: "description",
        required: false,
        help: "A sentence or two about the robot.",
    },
    FileKey {
        name: "contact",
        required: false,
        help: "Where to ask about the package, e.g. an email address or web page.",
    },
    FileKey {
        name: "urdf",
        required: true,
        help: "The robot description, relative to the package folder, e.g. `urdf/arm7.urdf`.",
    },
    FileKey {
        name: "streams",
        required: false,
        help: "What the robot sends, relative to the package folder. `streams.toml` by default.",
    },
    FileKey {
        name: "mapping",
        required: false,
        help: "Which decoded value drives which joint, relative to the package folder. `mapping.toml` by default.",
    },
];

/// A whole `package.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageManifest {
    /// The package's ID.
    pub id: String,
    /// Its version, as its author numbers them.
    pub version: String,
    /// What people call the robot.
    pub name: Option<String>,
    /// Who makes the robot or wrote the package.
    pub vendor: Option<String>,
    /// A sentence or two about the robot.
    pub description: Option<String>,
    /// Where to ask about the package.
    pub contact: Option<String>,
    /// The robot description, relative to the package folder.
    pub urdf: PathBuf,
    /// The stream declarations, relative to the package folder.
    pub streams: PathBuf,
    /// The field-to-joint mapping, relative to the package folder.
    pub mapping: PathBuf,
}

/// Reads the package description in `file`.
///
/// # Errors
///
/// Every problem found, each with the file and line: invalid TOML
/// (`config.invalid_syntax`), unknown keys (`package.unknown_key`), and
/// missing or invalid values (`config.invalid_value`), including paths that
/// lead out of the package.
pub fn parse_package_manifest(file: &ConfigFile) -> Result<PackageManifest, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let mut reader = Reader::new(file, document.as_table());
    reader.check_keys(
        &PACKAGE_KEYS,
        ErrorCode::PackageUnknownKey,
        &format!("`{PACKAGE_FILE}`"),
    );
    let id = reader.text("id", "an ID such as \"arm7\"", is_name);
    let version = reader.text("version", "text without spaces, e.g. \"1.2.0\"", |text| {
        !text.is_empty() && !text.contains(char::is_whitespace)
    });
    let [name, vendor, description, contact] =
        ["name", "vendor", "description", "contact"].map(|key| reader.text(key, "text", |_| true));
    let urdf = path(&mut reader, "urdf");
    let streams = path(&mut reader, "streams");
    let mapping = path(&mut reader, "mapping");
    if !reader.errors.is_empty() {
        return Err(reader.errors);
    }
    match (id, version, urdf) {
        (Some(id), Some(version), Some(urdf)) => Ok(PackageManifest {
            id,
            version,
            name,
            vendor,
            description,
            contact,
            urdf,
            streams: streams.unwrap_or_else(|| PathBuf::from("streams.toml")),
            mapping: mapping.unwrap_or_else(|| PathBuf::from("mapping.toml")),
        }),
        // Each missing one was reported above.
        _ => Err(reader.errors),
    }
}

/// A path inside the package at `key`.
fn path(reader: &mut Reader<'_>, key: &str) -> Option<PathBuf> {
    let text = reader.text(
        key,
        "a path inside the package, e.g. \"urdf/robot.urdf\"",
        |text| !text.is_empty(),
    )?;
    let path = PathBuf::from(&text);
    if stays_inside(&path) {
        Some(path)
    } else {
        let error = KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
            "`{key}` is \"{text}\", which leads out of the package; give a path inside its folder"
        ));
        reader.located(error, key);
        None
    }
}

/// Whether `path` names something inside the folder it is relative to.
fn stays_inside(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(contents: &str) -> Result<PackageManifest, Vec<KaptoError>> {
        parse_package_manifest(&ConfigFile {
            path: PathBuf::from("/packages/arm7/package.toml"),
            contents: contents.to_owned(),
        })
    }

    fn problems(contents: &str) -> Vec<String> {
        parse(contents)
            .unwrap_err()
            .iter()
            .map(|error| {
                let line = error
                    .context_entries()
                    .iter()
                    .find(|(key, _)| *key == "line")
                    .map(|(_, line)| line.clone())
                    .unwrap_or_default();
                format!("{} {line}: {}", error.code().id(), error.message())
            })
            .collect()
    }

    #[test]
    fn a_package_needs_only_its_id_version_and_robot_description() {
        let manifest =
            parse("id = \"arm7\"\nversion = \"0.1.0\"\nurdf = \"urdf/arm7.urdf\"\n").unwrap();
        assert_eq!(
            manifest,
            PackageManifest {
                id: "arm7".to_owned(),
                version: "0.1.0".to_owned(),
                name: None,
                vendor: None,
                description: None,
                contact: None,
                urdf: PathBuf::from("urdf/arm7.urdf"),
                streams: PathBuf::from("streams.toml"),
                mapping: PathBuf::from("mapping.toml"),
            }
        );
    }

    #[test]
    fn every_key_is_read() {
        let manifest = parse(
            "id = \"cobot\"\nversion = \"2024.3-rc1\"\nname = \"A cobot\"\nvendor = \"Acme\"\n\
             description = \"Six joints.\"\ncontact = \"robots@acme.example\"\n\
             urdf = \"robot.urdf\"\nstreams = \"config/streams.toml\"\nmapping = \"./config/mapping.toml\"\n",
        )
        .unwrap();
        assert_eq!(manifest.name.as_deref(), Some("A cobot"));
        assert_eq!(manifest.vendor.as_deref(), Some("Acme"));
        assert_eq!(manifest.description.as_deref(), Some("Six joints."));
        assert_eq!(manifest.contact.as_deref(), Some("robots@acme.example"));
        assert_eq!(manifest.streams, Path::new("config/streams.toml"));
        assert_eq!(manifest.mapping, Path::new("./config/mapping.toml"));
    }

    #[test]
    fn every_problem_is_reported_with_its_line() {
        assert_eq!(
            problems(
                "id = \"Arm-7\"\nversion = \"1 2\"\nvendr = \"Acme\"\nurdf = \"../other/robot.urdf\"\n\
                 streams = \"/etc/streams.toml\"\nname = 3\n"
            ),
            [
                "package.unknown_key 3: Unknown key in package.toml: `vendr` (did you mean `vendor`?)",
                "config.invalid_value 1: Invalid configuration value: `id` must be an ID such as \"arm7\"",
                "config.invalid_value 2: Invalid configuration value: `version` must be text without spaces, e.g. \"1.2.0\"",
                "config.invalid_value 6: Invalid configuration value: `name` must be text",
                "config.invalid_value 4: Invalid configuration value: `urdf` is \"../other/robot.urdf\", which leads out of the package; give a path inside its folder",
                "config.invalid_value 5: Invalid configuration value: `streams` is \"/etc/streams.toml\", which leads out of the package; give a path inside its folder",
            ]
        );
        assert_eq!(
            problems("name = \"x\"\n"),
            [
                "config.invalid_value 1: Invalid configuration value: `package.toml` needs `id`",
                "config.invalid_value 1: Invalid configuration value: `package.toml` needs `version`",
                "config.invalid_value 1: Invalid configuration value: `package.toml` needs `urdf`",
            ]
        );
        assert_eq!(
            problems("id = \n"),
            [
                "config.invalid_syntax 1: Configuration file is not valid TOML: string values must be quoted, expected literal string"
            ]
        );
    }

    #[test]
    fn the_unknown_key_remedy_names_every_key() {
        let remedy = ErrorCode::PackageUnknownKey.remedy();
        for key in PACKAGE_KEYS {
            assert!(remedy.contains(&format!("`{}`", key.name)), "{}", key.name);
        }
    }
}
