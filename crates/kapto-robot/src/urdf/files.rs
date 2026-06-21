// SPDX-License-Identifier: Apache-2.0

//! Finding the mesh and texture files a URDF refers to.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::Resource;

/// Finds files the way ROS does; see [`super::parse_urdf`].
pub(super) struct Finder<'a> {
    /// The URDF's folder.
    base: PathBuf,
    search_paths: &'a [PathBuf],
    /// The ROS package name each folder's `package.xml` declares, read once.
    declared: RefCell<HashMap<PathBuf, Option<String>>>,
}

impl<'a> Finder<'a> {
    pub(super) fn new(urdf: &Path, search_paths: &'a [PathBuf]) -> Self {
        Self {
            base: urdf.parent().unwrap_or(Path::new("")).to_path_buf(),
            search_paths,
            declared: RefCell::default(),
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
            // The URDF's folder as written and as it really is: a package
            // linked into the packages folder under another name is found
            // by the folder it links to.
            let real = std::fs::canonicalize(&self.base).ok();
            let folders = self
                .search_paths
                .iter()
                .map(PathBuf::as_path)
                .chain(self.base.ancestors())
                .chain(real.iter().flat_map(|real| real.ancestors()));
            for folder in folders {
                let mut candidates = vec![folder.join(package).join(inside)];
                // ROS finds a package by the name its package.xml declares,
                // whatever its folder is called.
                if folder.file_name().is_some_and(|name| name == package)
                    || self.declares(folder, package)
                {
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

    /// Whether `folder`'s `package.xml` declares the ROS package `package`.
    fn declares(&self, folder: &Path, package: &str) -> bool {
        self.declared
            .borrow_mut()
            .entry(folder.to_path_buf())
            .or_insert_with(|| declared_name(folder))
            .as_deref()
            == Some(package)
    }
}

/// The package name `folder`'s `package.xml` declares, if it has one.
fn declared_name(folder: &Path) -> Option<String> {
    let text = std::fs::read_to_string(folder.join("package.xml")).ok()?;
    let document = roxmltree::Document::parse(&text).ok()?;
    let root = document.root_element();
    (root.tag_name().name() == "package").then_some(())?;
    root.children()
        .find(|child| child.has_tag_name("name"))
        .and_then(|name| name.text())
        .map(|name| name.trim().to_owned())
}
