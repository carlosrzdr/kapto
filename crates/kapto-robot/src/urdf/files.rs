// SPDX-License-Identifier: Apache-2.0

//! Finding the mesh and texture files a URDF refers to.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::Resource;

/// Finds files the way ROS does; see [`super::parse_urdf`].
pub(crate) struct Finder<'a> {
    /// The URDF's folder.
    base: PathBuf,
    search_paths: &'a [PathBuf],
    /// The ROS package name each folder's `package.xml` declares, read once.
    declared: RefCell<HashMap<PathBuf, Option<String>>>,
    /// Each search path's folders, read once.
    children: RefCell<HashMap<PathBuf, Vec<PathBuf>>>,
}

impl<'a> Finder<'a> {
    pub(crate) fn new(urdf: &Path, search_paths: &'a [PathBuf]) -> Self {
        Self {
            base: urdf.parent().unwrap_or(Path::new("")).to_path_buf(),
            search_paths,
            declared: RefCell::default(),
            children: RefCell::default(),
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
            return self
                .package_folders(package)
                .into_iter()
                .map(|folder| folder.join(inside))
                .find(|path| path.is_file());
        }
        let path = Path::new(uri.strip_prefix("file://").unwrap_or(uri));
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base.join(path)
        };
        path.is_file().then_some(path)
    }

    /// Where a package is looked for: each search path, then each folder
    /// above the URDF, as written and as it really is. A package linked
    /// into the packages folder under another name is found by the folder
    /// it links to.
    fn folders(&self) -> Vec<PathBuf> {
        let real = std::fs::canonicalize(&self.base).ok();
        self.search_paths
            .iter()
            .map(PathBuf::as_path)
            .chain(self.base.ancestors())
            .chain(real.iter().flat_map(|real| real.ancestors()))
            .map(Path::to_path_buf)
            .collect()
    }

    /// The folder of the ROS package `package`, found as its files are.
    pub(crate) fn package_folder(&self, package: &str) -> Option<PathBuf> {
        self.package_folders(package)
            .into_iter()
            .find(|folder| folder.is_dir())
    }

    /// The folders that may be the ROS package `package`, in the order they
    /// are looked in: in each of [`Finder::folders`], a folder called
    /// `package`, the folder itself if it is called so or its
    /// `package.xml` declares that name, and, in a search path, each folder
    /// whose `package.xml` declares it, as ROS finds packages in a
    /// workspace whatever their folders are called.
    fn package_folders(&self, package: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for folder in self.folders() {
            found.push(folder.join(package));
            if folder.file_name().is_some_and(|name| name == package)
                || self.declares(&folder, package)
            {
                found.push(folder.clone());
            }
            if self.search_paths.contains(&folder) {
                found.extend(
                    self.subfolders(&folder)
                        .into_iter()
                        .filter(|child| self.declares(child, package)),
                );
            }
        }
        found
    }

    /// The folders in `folder`, read once.
    fn subfolders(&self, folder: &Path) -> Vec<PathBuf> {
        self.children
            .borrow_mut()
            .entry(folder.to_path_buf())
            .or_insert_with(|| {
                std::fs::read_dir(folder)
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .clone()
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
