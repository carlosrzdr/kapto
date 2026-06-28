// SPDX-License-Identifier: Apache-2.0

//! The live view's layout as it changes while serve runs: the package's own
//! (or the one made from it), unless a default saved for the package in the
//! configuration folder comes first. Saving writes that default as a file,
//! as configuration is kept; removing it brings back the package's.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use kapto_config::{ConfigFile, default_layout_path, parse_layout};
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::Declared;
use kapto_viz::page_layout::{LayoutInput, ViewLayout, view_of};

/// The layouts the live view may be arranged by.
#[derive(Clone, Debug, Default)]
pub struct Layouts(Arc<Mutex<Option<Arranged>>>);

#[derive(Debug)]
struct Arranged {
    /// What the package declares, for what a layout names that it lacks.
    declared: Declared,
    /// The package's own layout, or the one made from it.
    package: ViewLayout,
    /// The default saved in the configuration folder, if there is one.
    saved: Option<ViewLayout>,
    /// Where it is saved: none without a package.
    file: Option<PathBuf>,
}

impl Layouts {
    /// The layouts of the package `id` runs, if it runs one: its `package`
    /// layout, and the default saved for it in `config_folder` if there is
    /// one that reads. Returns the problems of one that doesn't.
    #[must_use]
    pub fn load(
        config_folder: &Path,
        id: Option<&str>,
        declared: Declared,
        package: ViewLayout,
    ) -> (Self, Vec<KaptoError>) {
        let file = id.map(|id| default_layout_path(config_folder, id));
        let (saved, problems) = match file.as_deref().map(std::fs::read_to_string) {
            Some(Ok(contents)) => {
                let read = ConfigFile {
                    path: file.clone().unwrap_or_default(),
                    contents,
                };
                match parse_layout(&read) {
                    Ok(layout) => (Some(view_of(&layout, &declared, "default")), Vec::new()),
                    Err(problems) => (None, problems),
                }
            }
            _ => (None, Vec::new()),
        };
        let arranged = Arranged {
            declared,
            package,
            saved,
            file,
        };
        (Self(Arc::new(Mutex::new(Some(arranged)))), problems)
    }

    /// The layout in use: the saved default, else the package's; none
    /// before [`Layouts::load`].
    #[must_use]
    pub fn current(&self) -> Option<ViewLayout> {
        let arranged = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        arranged.as_ref().map(|arranged| {
            arranged
                .saved
                .clone()
                .unwrap_or_else(|| arranged.package.clone())
        })
    }

    /// Saves `input` as the package's default layout, and arranges by it.
    ///
    /// # Errors
    ///
    /// `layout.without_package` without a package to save it for; the
    /// layout's problems, as reading `layout.toml` gives them;
    /// `layout.write_failed` if the file can't be written.
    pub fn save(&self, input: &LayoutInput) -> Result<ViewLayout, Vec<KaptoError>> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (arranged, file) = package_file(guard.as_mut())?;
        let layout = input.to_layout(&file)?;
        write(&file, &layout.to_toml()).map_err(|error| vec![error])?;
        let saved = view_of(&layout, &arranged.declared, "default");
        arranged.saved = Some(saved.clone());
        Ok(saved)
    }

    /// `input` as the view draws it, each panel with what the robot's
    /// streams lack of it: for a layout the operator keeps in their
    /// browser, so the page says what it lacks by the rules every other
    /// layout is held to.
    ///
    /// # Errors
    ///
    /// The layout's problems, as reading `layout.toml` gives them;
    /// `layout.without_package` before the layouts are loaded.
    pub fn check(&self, input: &LayoutInput) -> Result<ViewLayout, Vec<KaptoError>> {
        let guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let arranged = guard.as_ref().ok_or_else(|| vec![without_package()])?;
        let layout = input.to_layout(Path::new("the page's layout"))?;
        Ok(view_of(&layout, &arranged.declared, "own"))
    }

    /// Removes the package's default layout, and arranges by the package's.
    ///
    /// # Errors
    ///
    /// `layout.without_package` without a package; `layout.write_failed`
    /// if the file can't be removed.
    pub fn remove(&self) -> Result<ViewLayout, Vec<KaptoError>> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (arranged, file) = package_file(guard.as_mut())?;
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(vec![write_failed(&file, &error)]),
        }
        arranged.saved = None;
        Ok(arranged.package.clone())
    }
}

/// The layouts and where the package's default is saved.
fn package_file(
    arranged: Option<&mut Arranged>,
) -> Result<(&mut Arranged, PathBuf), Vec<KaptoError>> {
    match arranged {
        Some(arranged) => match arranged.file.clone() {
            Some(file) => Ok((arranged, file)),
            None => Err(vec![without_package()]),
        },
        None => Err(vec![without_package()]),
    }
}

fn without_package() -> KaptoError {
    KaptoError::new(ErrorCode::LayoutWithoutPackage)
        .detail("serve's streams aren't a robot package's".to_owned())
}

/// Writes `text` to `file` whole: beside it first, then renamed over it.
fn write(file: &Path, text: &str) -> Result<(), KaptoError> {
    let partial = file.with_extension("toml.partial");
    file.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&partial, text))
        .and_then(|()| std::fs::rename(&partial, file))
        .map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            write_failed(file, &error)
        })
}

fn write_failed(file: &Path, error: &std::io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::LayoutWriteFailed)
        .detail(error.to_string())
        .context("file", file.display().to_string())
}
