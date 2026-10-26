// SPDX-License-Identifier: Apache-2.0

//! The live view's layouts as they change while serve runs: the package's
//! own (or the one made from it), and the named layouts kept for the
//! package in the configuration folder, `layouts/<package id>/<file>.toml`,
//! each a file as configuration is kept. `default.toml` among them is the
//! package's *Default*, which comes before its own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use kapto_config::{
    ConfigFile, DEFAULT_NAMED_LAYOUT, Layout, check_layout_name, layout_file_stem,
    package_layouts_folder, parse_layout, parse_named_layout, renamed_layout,
};
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::{Declared, made_layout};
use kapto_viz::page_layout::{ColumnInput, LayoutInput, PanelInput, ViewLayout, view_of};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::ApiError;

/// The name *Default* is given when it is first saved.
const DEFAULT_NAME: &str = "Default";

/// One of a package's named layouts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
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

/// A package's named layouts, *Default* first and the rest by name, and
/// the two that follow the package, which the page offers beside them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NamedLayouts {
    /// The ones that read.
    pub layouts: Vec<NamedLayout>,
    /// Why each of the others doesn't, with its file and line.
    pub problems: Vec<ApiError>,
    /// The package's own `layout.toml`, if it has one that reads.
    pub package: Option<ViewLayout>,
    /// The one made from what the package declares.
    pub made: ViewLayout,
}

/// A layout as a file of its own, as it is exported and imported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LayoutFile {
    /// The file's name, e.g. `wrist-view.toml`.
    pub file: String,
    /// Its text, as `layout.toml` holds a layout.
    pub text: String,
}

/// A layout to be saved under a name of its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NamedLayoutInput {
    /// Its name: at most 64 characters, with a letter or digit, unique
    /// among the package's layouts. Its file is named from it.
    pub name: String,
    /// Its columns, left to right.
    pub columns: Vec<ColumnInput>,
}

/// A named layout's new name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LayoutName {
    /// The name, held to the rules of [`NamedLayoutInput::name`]. Its file
    /// moves to the one named from it.
    pub name: String,
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
            list.package = (arranged.package.source == "package").then(|| arranged.package.clone());
            list.made = view_of(&made_layout(&arranged.declared), &arranged.declared, "made");
            Ok(list)
        })
    }

    /// Saves `input` as a new named layout, in the file named from its name.
    ///
    /// # Errors
    ///
    /// `layout.name_empty`, `layout.name_too_long` or `layout.name_taken`
    /// for its name; the layout's problems; `layout.without_package`;
    /// `layout.write_failed`.
    pub fn create(&self, input: &NamedLayoutInput) -> Result<NamedLayout, Problems> {
        self.with(|arranged, folder| {
            let id = free_stem(folder, &input.name, None)?;
            let layout = LayoutInput {
                columns: input.columns.clone(),
            };
            arranged.save(folder, &id, input.name.trim(), &layout)
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
                None if id == DEFAULT_NAMED_LAYOUT => {
                    free_stem(folder, DEFAULT_NAME, None)?;
                    DEFAULT_NAME.to_owned()
                }
                None => return Err(vec![not_found(id)]),
            };
            arranged.save(folder, id, &name, input)
        })
    }

    /// Renames the named layout `id`, its file moved to the one named from
    /// `name` and otherwise kept as written.
    ///
    /// # Errors
    ///
    /// `layout.not_found`; the problems of its file if it doesn't read;
    /// `layout.name_empty`, `layout.name_too_long` or `layout.name_taken`;
    /// `layout.without_package`; `layout.write_failed`.
    pub fn rename(&self, id: &str, name: &str) -> Result<NamedLayout, Problems> {
        self.with(|arranged, folder| {
            let file = file_of(folder, id);
            let Some((layout, _)) = read(&file)? else {
                return Err(vec![not_found(id)]);
            };
            let name = name.trim();
            let new = free_stem(folder, name, Some(id))?;
            let text =
                std::fs::read_to_string(&file).map_err(|error| vec![failed(&file, &error)])?;
            let text = renamed_layout(&text, name).ok_or_else(|| vec![not_found(id)])?;
            move_renamed(&file, &file_of(folder, &new), &text).map_err(|error| vec![error])?;
            arranged.forget(id);
            let renamed = Layout {
                name: Some(name.to_owned()),
                ..layout
            };
            Ok(arranged.keep(new, name.to_owned(), &renamed))
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
    /// ones, one change at a time, so a name can't be taken twice.
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

/// `input` as a file of its own, named from its name, its text starting
/// with it as a named layout's does; nothing is saved.
///
/// # Errors
///
/// `layout.name_empty` or `layout.name_too_long` for its name; the
/// layout's problems.
pub fn exported(input: &NamedLayoutInput) -> Result<LayoutFile, Problems> {
    let name = input.name.trim();
    check_layout_name(name).map_err(|error| vec![error])?;
    let file = format!("{}.toml", layout_file_stem(name));
    let layout = LayoutInput {
        columns: input.columns.clone(),
    }
    .to_layout(Path::new(&file))?;
    let text = Layout {
        name: Some(name.to_owned()),
        ..layout
    }
    .to_toml();
    Ok(LayoutFile { file, text })
}

/// The layout in `file`, read as `layout.toml` is, named as it names
/// itself, else from the file's name, so a package's own imports as well
/// as an exported one. Nothing is saved; its name is checked when it is.
///
/// # Errors
///
/// The file's problems, each with its line.
pub fn imported(file: &LayoutFile) -> Result<NamedLayoutInput, Problems> {
    let layout = parse_layout(&ConfigFile {
        path: PathBuf::from(&file.file),
        contents: file.text.clone(),
    })?;
    let name = layout.name.clone().unwrap_or_else(|| {
        let base = file.file.rsplit(['/', '\\']).next().unwrap_or_default();
        base.strip_suffix(".toml").unwrap_or(base).to_owned()
    });
    let columns = view_of(&layout, &Declared::default(), "own")
        .columns
        .into_iter()
        .map(|column| ColumnInput {
            width: column.width,
            panels: column
                .panels
                .into_iter()
                .map(|panel| PanelInput {
                    kind: panel.kind,
                    height: panel.height,
                    stream: panel.stream,
                    fields: panel.fields,
                    title: panel.title,
                    view: panel.view,
                })
                .collect(),
        })
        .collect();
    Ok(NamedLayoutInput { name, columns })
}

/// The file stem `name` is saved as, unless it breaks a rule of a name or
/// another of the layouts in `folder` (but `except`) has it or its file.
fn free_stem(folder: &Path, name: &str, except: Option<&str>) -> Result<String, Problems> {
    check_layout_name(name).map_err(|error| vec![error])?;
    let stem = layout_file_stem(name);
    for (id, read) in files(folder)? {
        let theirs = match read {
            Ok(Some((_, name))) => layout_file_stem(&name),
            _ => String::new(),
        };
        if Some(id.as_str()) != except && (id == stem || theirs == stem) {
            return Err(vec![
                KaptoError::new(ErrorCode::LayoutNameTaken)
                    .detail(format!("`{}` is taken by `{id}.toml`", name.trim())),
            ]);
        }
    }
    Ok(stem)
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

/// Moves the layout in `file` to `moved`, holding `text`: written over it
/// whole first, then moved by one rename, so a crash or failure between
/// leaves the layout once, under its old file with its new name, never in
/// two files.
fn move_renamed(file: &Path, moved: &Path, text: &str) -> Result<(), KaptoError> {
    write(file, text)?;
    if moved == file {
        return Ok(());
    }
    std::fs::rename(file, moved)
        .and_then(|()| match moved.parent() {
            Some(folder) => std::fs::File::open(folder).and_then(|folder| folder.sync_all()),
            None => Ok(()),
        })
        .map_err(|error| failed(moved, &error))
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
    where_in_file(&format!("{id}.toml"), problem)
}

/// The file `file` and the line `problem` is at, e.g. `` `night.toml` line 2 ``.
pub(crate) fn where_in_file(file: &str, problem: &KaptoError) -> String {
    let line = problem
        .context_entries()
        .iter()
        .find(|(key, _)| *key == "line")
        .map(|(_, line)| format!(" line {line}"));
    format!("`{file}`{}", line.unwrap_or_default())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_rename_that_cant_move_the_file_leaves_the_layout_once_under_its_old_file() {
        let folder = kapto_testkit::scratch("layouts-move-fails");
        let file = folder.join("wrist.toml");
        std::fs::write(&file, "name = \"Wrist\"\n").unwrap();
        // The move's target is in a folder that isn't there: the rename fails
        // after the new name is written.
        let moved = folder.join("missing/gripper.toml");
        let error = move_renamed(&file, &moved, "name = \"Gripper\"\n").unwrap_err();
        assert_eq!(error.code(), ErrorCode::LayoutWriteFailed);
        assert_eq!(entries(&folder), ["wrist.toml"]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "name = \"Gripper\"\n"
        );

        move_renamed(&file, &folder.join("gripper.toml"), "name = \"Gripper\"\n").unwrap();
        assert_eq!(entries(&folder), ["gripper.toml"]);
    }
}
