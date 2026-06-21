// SPDX-License-Identifier: Apache-2.0

//! Robot descriptions in xacro, expanded to URDF: macros, properties,
//! Python expressions, includes, `load_yaml` and the arguments they are
//! given, by `xacro-rs`, which matches ROS's own xacro on the vendors'
//! descriptions it was checked against. `$(find <package>)` is the folder
//! Kapto finds the package in, the way it finds a URDF's `package://` files.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};
use xacro_rs::XacroProcessor;
use xacro_rs::extensions::ExtensionHandler;

use crate::urdf::files::Finder;

/// The URDF the xacro file at `path` expands to with `args` (`name:=value`
/// on ROS's command line), its `$(find)` packages looked for in
/// `search_paths` and the folders above each file it reads.
///
/// # Errors
///
/// `robot.xacro_failed`, saying where and why, if it doesn't expand.
pub fn expand_xacro(
    path: &Path,
    args: &BTreeMap<String, String>,
    search_paths: &[PathBuf],
) -> Result<String, KaptoError> {
    let find = FindPackage {
        search_paths: search_paths.to_vec(),
        current: RefCell::new(path.to_path_buf()),
    };
    XacroProcessor::builder()
        .with_args(args.clone().into_iter().collect())
        .with_extension(Box::new(find))
        .with_ros_yaml_units()
        .build()
        .run(path)
        .map_err(|error| {
            KaptoError::new(ErrorCode::RobotXacroFailed)
                .detail(error.to_string())
                .context("file", path.display().to_string())
        })
}

/// `$(find <package>)`, answered as Kapto finds packages.
struct FindPackage {
    search_paths: Vec<PathBuf>,
    /// The file being read: its folders are looked in too.
    current: RefCell<PathBuf>,
}

impl ExtensionHandler for FindPackage {
    fn resolve(&self, command: &str, args_raw: &str) -> Result<Option<String>, Box<dyn Error>> {
        if command != "find" {
            return Ok(None);
        }
        let package = args_raw.trim();
        let current = self.current.borrow();
        Finder::new(&current, &self.search_paths)
            .package_folder(package)
            .map(|folder| Some(folder.display().to_string()))
            .ok_or_else(|| {
                format!(
                    "no package `{package}` beside {} or in the packages folder",
                    current.display()
                )
                .into()
            })
    }

    fn on_file_change(&self, current_file: Option<&Path>) {
        if let Some(file) = current_file {
            file.clone_into(&mut self.current.borrow_mut());
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
