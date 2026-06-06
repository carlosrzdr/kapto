// SPDX-License-Identifier: Apache-2.0

//! Finding the mesh and texture files a URDF refers to.

use std::path::{Path, PathBuf};

use crate::model::Resource;

/// Finds files the way ROS does; see [`super::parse_urdf`].
pub(super) struct Finder<'a> {
    /// The URDF's folder.
    base: PathBuf,
    search_paths: &'a [PathBuf],
}

impl<'a> Finder<'a> {
    pub(super) fn new(urdf: &Path, search_paths: &'a [PathBuf]) -> Self {
        Self {
            base: urdf.parent().unwrap_or(Path::new("")).to_path_buf(),
            search_paths,
        }
    }

    pub(super) fn resource(&self, uri: &str) -> Resource {
        Resource {
            uri: uri.to_owned(),
            path: self.find(uri),
        }
    }

    fn find(&self, uri: &str) -> Option<PathBuf> {
        if let Some(rest) = uri.strip_prefix("package://") {
            let (package, inside) = rest.split_once('/')?;
            let folders = self
                .search_paths
                .iter()
                .map(PathBuf::as_path)
                .chain(self.base.ancestors());
            for folder in folders {
                let mut candidates = vec![folder.join(package).join(inside)];
                if folder.file_name().is_some_and(|name| name == package) {
                    candidates.push(folder.join(inside));
                }
                if let Some(found) = candidates.into_iter().find(|path| path.is_file()) {
                    return Some(found);
                }
            }
            return None;
        }
        let path = Path::new(uri.strip_prefix("file://").unwrap_or(uri));
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base.join(path)
        };
        path.is_file().then_some(path)
    }
}
