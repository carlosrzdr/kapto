// SPDX-License-Identifier: Apache-2.0

//! The live view's layouts as they change while serve runs: the package's
//! own (or the one made from it), and the named layouts kept for the
//! package in the configuration folder, `layouts/<package id>/<file>.toml`,
//! each a file as configuration is kept. `default.toml` among them is the
//! package's *Default*, which comes before its own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use kapto_config::{
    ConfigFile, DEFAULT_NAMED_LAYOUT, Layout, package_layouts_folder, parse_named_layout,
};
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::Declared;
use kapto_viz::page_layout::{LayoutInput, ViewLayout, view_of};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::ApiError;

/// The name *Default* is given when it is first saved.
const DEFAULT_NAME: &str = "Default";

/// One of a package's named layouts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NamedLayout {
    /// Its file's name without `.toml`, which the API asks for it by:
    /// `default` for the package's *Default*.
    pub id: String,
    /// Its name, as its file gives it.
    pub name: String,
    /// It as the view draws it, each panel with what the package lacks of
    /// it; `source` is `default` for *Default*, else `named`.
    pub layout: ViewLayout,
}

/// A package's named layouts, *Default* first and the rest by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NamedLayouts {
    /// The ones that read.
    pub layouts: Vec<NamedLayout>,
    /// Why each of the others doesn't, with its file and line.
    pub problems: Vec<ApiError>,
}

/// The layouts the live view may be arranged by.
#[derive(Clone, Debug, Default)]
pub struct Layouts(Arc<Mutex<Option<Arranged>>>);

#[derive(Debug)]
struct Arranged {
    /// What the package declares, for what a layout names that it lacks.
    declared: Declared,
    /// The package's own layout, or the one made from it.
    package: ViewLayout,
    /// The package's *Default*, if it has one that reads.
    default: Option<ViewLayout>,
    /// The folder of the package's named layouts: none without a package.
    folder: Option<PathBuf>,
}

type Problems = Vec<KaptoError>;

/// A named layout and its name as its file gives them, none without the
/// file, or why it does not read.
type Read = Result<Option<(Layout, String)>, Problems>;

impl Layouts {
    /// The layouts of the package `id` runs, if it runs one: its `package`
    /// layout, and its *Default* in `config_folder` if it has one that
    /// reads. Returns the problems of one that doesn't.
    #[must_use]
    pub fn load(
        config_folder: &Path,
        id: Option<&str>,
        declared: Declared,
        package: ViewLayout,
    ) -> (Self, Problems) {
        let folder = id.map(|id| package_layouts_folder(config_folder, id));
        let file = folder
            .as_deref()
            .map(|folder| file_of(folder, DEFAULT_NAMED_LAYOUT));
        let (default, problems) = match file.as_deref().map(read) {
            Some(Ok(Some((layout, _)))) => {
                (Some(view_of(&layout, &declared, "default")), Vec::new())
            }
            Some(Err(problems)) => (None, problems),
            _ => (None, Vec::new()),
        };
        let arranged = Arranged {
            declared,
            package,
            default,
            folder,
        };
        (Self(Arc::new(Mutex::new(Some(arranged)))), problems)
    }

    /// The layout in use: *Default*, else the package's; none before
    /// [`Layouts::load`].
    #[must_use]
    pub fn current(&self) -> Option<ViewLayout> {
        let arranged = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        arranged.as_ref().map(Arranged::current)
    }

    /// `input` as the view draws it, each panel with what the robot's
    /// streams lack of it, by the rules every other layout is held to;
    /// nothing is saved.
    ///
    /// # Errors
    ///
    /// The layout's problems, as reading `layout.toml` gives them;
    /// `layout.without_package` before the layouts are loaded.
    pub fn check(&self, input: &LayoutInput) -> Result<ViewLayout, Problems> {
        let guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let arranged = guard.as_ref().ok_or_else(|| vec![without_package()])?;
        let layout = input.to_layout(Path::new("the page's layout"))?;
        Ok(view_of(&layout, &arranged.declared, "own"))
    }

    /// The package's named layouts as their files hold them now, hand
    /// edits included.
    ///
    /// # Errors
    ///
    /// `layout.without_package` without a package; `layout.write_failed`
    /// if the folder can't be read.
    pub fn list(&self) -> Result<NamedLayouts, Problems> {
        self.with(|arranged, folder| {
            let mut list = NamedLayouts::default();
            for (id, read) in files(folder)? {
                match read {
                    Ok(Some((layout, name))) => {
                        list.layouts.push(arranged.named(id, name, &layout));
                    }
                    Ok(None) => {}
                    Err(problems) => {
                        list.problems.extend(problems.iter().map(|problem| {
                            let mut said = ApiError::from(problem);
                            said.message = format!("{}: {}", where_in(&id, problem), said.message);
                            said
                        }));
                    }
                }
            }
            list.layouts
                .sort_by_key(|each| (each.id != DEFAULT_NAMED_LAYOUT, each.name.to_lowercase()));
            Ok(list)
        })
    }

    /// Saves `input` as the named layout `id`, keeping its name; `default`,
    /// the package's *Default*, is made if there is none.
    ///
    /// # Errors
    ///
    /// `layout.not_found` if there is no such layout; the problems of its
    /// file if it doesn't read; the layout's problems;
    /// `layout.without_package`; `layout.write_failed`.
    pub fn write(&self, id: &str, input: &LayoutInput) -> Result<NamedLayout, Problems> {
        self.with(|arranged, folder| {
            let name = match read(&file_of(folder, id))? {
                Some((_, name)) => name,
                None if id == DEFAULT_NAMED_LAYOUT => DEFAULT_NAME.to_owned(),
                None => return Err(vec![not_found(id)]),
            };
            arranged.save(folder, id, &name, input)
        })
    }

    /// Deletes the named layout `id`, if it is there, and answers with the
    /// layout the view is now arranged by.
    ///
    /// # Errors
    ///
    /// `layout.without_package`; `layout.write_failed` if the file can't
    /// be removed.
    pub fn remove(&self, id: &str) -> Result<ViewLayout, Problems> {
        self.with(|arranged, folder| {
            if valid_id(id) {
                remove(&file_of(folder, id))?;
            }
            arranged.forget(id);
            Ok(arranged.current())
        })
    }

    /// Runs `work` on the layouts and the folder of the package's named
    /// ones, one change at a time.
    fn with<T>(
        &self,
        work: impl FnOnce(&mut Arranged, &Path) -> Result<T, Problems>,
    ) -> Result<T, Problems> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let arranged = guard.as_mut().ok_or_else(|| vec![without_package()])?;
        let folder = arranged
            .folder
            .clone()
            .ok_or_else(|| vec![without_package()])?;
        work(arranged, &folder)
    }
}

impl Arranged {
    fn current(&self) -> ViewLayout {
        self.default.clone().unwrap_or_else(|| self.package.clone())
    }

    fn named(&self, id: String, name: String, layout: &Layout) -> NamedLayout {
        let source = if id == DEFAULT_NAMED_LAYOUT {
            "default"
        } else {
            "named"
        };
        NamedLayout {
            id,
            name,
            layout: view_of(layout, &self.declared, source),
        }
    }

    /// `named` for `layout`, kept as *Default* if it is.
    fn keep(&mut self, id: String, name: String, layout: &Layout) -> NamedLayout {
        let named = self.named(id, name, layout);
        if named.id == DEFAULT_NAMED_LAYOUT {
            self.default = Some(named.layout.clone());
        }
        named
    }

    fn forget(&mut self, id: &str) {
        if id == DEFAULT_NAMED_LAYOUT {
            self.default = None;
        }
    }

    /// Writes `input` named `name` as the layout `id`.
    fn save(
        &mut self,
        folder: &Path,
        id: &str,
        name: &str,
        input: &LayoutInput,
    ) -> Result<NamedLayout, Problems> {
        let file = file_of(folder, id);
        let layout = Layout {
            name: Some(name.to_owned()),
            ..input.to_layout(&file)?
        };
        write(&file, &layout.to_toml()).map_err(|error| vec![error])?;
        Ok(self.keep(id.to_owned(), name.to_owned(), &layout))
    }
}

/// Every `.toml` file in `folder`, by stem, as it reads; none if there is no
/// folder.
fn files(folder: &Path) -> Result<Vec<(String, Read)>, Problems> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(vec![failed(folder, &error)]),
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = path.file_stem().and_then(|stem| stem.to_str());
        if let (Some(stem), Some("toml")) = (stem, path.extension().and_then(|it| it.to_str())) {
            files.push((stem.to_owned(), read(&path)));
        }
    }
    Ok(files)
}

/// The named layout in `file` and its name; none if there is no file.
fn read(file: &Path) -> Read {
    let contents = match std::fs::read_to_string(file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(vec![failed(file, &error)]),
    };
    let layout = parse_named_layout(&ConfigFile {
        path: file.to_owned(),
        contents,
    })?;
    let name = layout.name.clone().unwrap_or_default();
    Ok(Some((layout, name)))
}

/// Where the layout `id` is in `folder`; one that can't be a file's stem
/// there is never found.
fn file_of(folder: &Path, id: &str) -> PathBuf {
    let stem = if valid_id(id) { id } else { "" };
    folder.join(format!("{stem}.toml"))
}

/// Whether `id` names a file of the folder itself, rather than one beside
/// or under it.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && !id.contains(['/', '\\'])
}

fn remove(file: &Path) -> Result<(), Problems> {
    match std::fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(vec![failed(file, &error)]),
    }
}

/// The file of layout `id` and the line `problem` is at, which an
/// [`ApiError`] doesn't carry, e.g. `` `night.toml` line 2 ``.
fn where_in(id: &str, problem: &KaptoError) -> String {
    let line = problem
        .context_entries()
        .iter()
        .find(|(key, _)| *key == "line")
        .map(|(_, line)| format!(" line {line}"));
    format!("`{id}.toml`{}", line.unwrap_or_default())
}

fn without_package() -> KaptoError {
    KaptoError::new(ErrorCode::LayoutWithoutPackage)
        .detail("serve's streams aren't a robot package's".to_owned())
}

fn not_found(id: &str) -> KaptoError {
    KaptoError::new(ErrorCode::LayoutNotFound).detail(format!("no layout `{id}`"))
}

/// Writes `text` to `file` whole, as episodes' own files are written: a
/// crash while saving leaves the layout as it was or as saved, never cut.
fn write(file: &Path, text: &str) -> Result<(), KaptoError> {
    file.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| kapto_store::write_whole(file, text.as_bytes()))
        .map_err(|error| failed(file, &error))
}

fn failed(file: &Path, error: &std::io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::LayoutWriteFailed)
        .detail(error.to_string())
        .context("file", file.display().to_string())
}
