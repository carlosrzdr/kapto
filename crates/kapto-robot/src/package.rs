// SPDX-License-Identifier: Apache-2.0

//! Robot packages: a folder with a `package.toml`, the robot's description,
//! its streams and their mapping to joints, and optionally the settings of
//! the quality checks on its episodes (`qa.toml`).
//!
//! A package is identified by its ID and version, as `package.toml` says,
//! and by its content hash, which changes whenever any of its files does.
//! The hash is SHA-256 over one line per file, `<sha256 of the file>  <path>`
//! with paths relative to the package folder, separated by `/`, in byte
//! order, each line ending in a newline: what `sha256sum` prints for the
//! files, sorted. Files and folders whose names start with a dot are left
//! out, so a `.git` folder or an editor's leftovers don't change it. The same
//! files give the same hash on every machine, whatever order the file system
//! lists them in:
//!
//! ```sh
//! cd packages/arm7
//! find . -type f ! -path '*/.*' | sed 's|^\./||' | LC_ALL=C sort | xargs sha256sum | sha256sum
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use kapto_config::{
    PACKAGE_FILE, PackageManifest, QA_FILE, QaSettings, parse_package_manifest, parse_qa,
    read_config_file,
};
use kapto_core::{ErrorCode, KaptoError};
use sha2::{Digest, Sha256};

/// A loaded robot package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// The folder that holds it.
    pub folder: PathBuf,
    /// What its `package.toml` says.
    pub manifest: PackageManifest,
    /// Every file in it, in hash order.
    pub files: Vec<PackageFile>,
    /// Its content hash, as lowercase hex.
    pub hash: String,
}

/// One file of a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageFile {
    /// Its path inside the package, separated by `/`.
    pub path: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// The SHA-256 of its contents, as lowercase hex.
    pub sha256: String,
}

impl Package {
    /// The robot description's path.
    #[must_use]
    pub fn urdf(&self) -> PathBuf {
        self.folder.join(&self.manifest.urdf)
    }

    /// The stream declarations' path.
    #[must_use]
    pub fn streams(&self) -> PathBuf {
        self.folder.join(&self.manifest.streams)
    }

    /// The field-to-joint mapping's path.
    #[must_use]
    pub fn mapping(&self) -> PathBuf {
        self.folder.join(&self.manifest.mapping)
    }

    /// The quality checks' settings' path: `qa.toml`, which may be absent.
    #[must_use]
    pub fn qa(&self) -> PathBuf {
        self.folder.join(QA_FILE)
    }

    /// The quality checks' settings: what `qa.toml` says, or every check
    /// with its defaults if the package has none.
    ///
    /// # Errors
    ///
    /// The problems of `qa.toml`, each with its line; `config.unreadable`
    /// if it can't be read.
    pub fn qa_settings(&self) -> Result<QaSettings, Vec<KaptoError>> {
        match read_config_file(&self.qa(), false).map_err(|error| vec![error])? {
            Some(file) => parse_qa(&file),
            None => Ok(QaSettings::default()),
        }
    }
}

/// The file extensions of meshes, and of the textures and buffers they
/// load: files that can be large, which a recording carries only when asked
/// to.
pub const MESH_EXTENSIONS: &[&str] = &[
    "bin", "bmp", "dae", "glb", "gltf", "jpeg", "jpg", "ktx2", "mtl", "obj", "ply", "png", "stl",
    "tga", "webp",
];

/// Whether the package file at `path` is a mesh, or a texture or buffer a
/// mesh loads, by its extension.
#[must_use]
pub fn is_mesh_or_texture(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| MESH_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

impl Package {
    /// The contents of each of its files for which `wanted` holds, read now
    /// and checked against the hashes taken when it was loaded, so they are
    /// the files its content hash stands for.
    ///
    /// # Errors
    ///
    /// `package.unreadable` if a file can't be read, or has changed since
    /// the package was loaded.
    pub fn contents(
        &self,
        wanted: impl Fn(&PackageFile) -> bool,
    ) -> Result<Vec<(&PackageFile, Vec<u8>)>, KaptoError> {
        self.files
            .iter()
            .filter(|file| wanted(file))
            .map(|file| {
                let path = self.folder.join(&file.path);
                let unreadable = |detail: String| {
                    KaptoError::new(ErrorCode::PackageUnreadable)
                        .detail(detail)
                        .context("path", path.display().to_string())
                };
                let data = std::fs::read(&path).map_err(|error| unreadable(error.to_string()))?;
                if hex(&Sha256::digest(&data)) != file.sha256 {
                    return Err(unreadable(
                        "it changed after the package was loaded; load it again".to_owned(),
                    ));
                }
                Ok((file, data))
            })
            .collect()
    }
}

/// Loads the package in `folder`: reads its `package.toml`, checks that the
/// files it names are there, and hashes its contents.
///
/// # Errors
///
/// Every problem found: `package.not_found` without a `package.toml`, the
/// errors of `package.toml` itself, `package.missing_file` for each file it
/// names that isn't there, `package.unsafe_path` for symbolic links and
/// names that aren't UTF-8, and `package.unreadable` if a file can't be read.
pub fn load_package(folder: &Path) -> Result<Package, Vec<KaptoError>> {
    let manifest_path = folder.join(PACKAGE_FILE);
    let file = read_config_file(&manifest_path, false)
        .map_err(|error| vec![error])?
        .ok_or_else(|| {
            vec![
                KaptoError::new(ErrorCode::PackageNotFound)
                    .detail(format!("{} has no {PACKAGE_FILE}", folder.display())),
            ]
        })?;
    let manifest = parse_package_manifest(&file)?;
    let mut errors = Vec::new();
    for (key, path) in [
        ("urdf", &manifest.urdf),
        ("streams", &manifest.streams),
        ("mapping", &manifest.mapping),
    ] {
        if !folder.join(path).is_file() {
            errors.push(
                KaptoError::new(ErrorCode::PackageMissingFile)
                    .detail(format!("`{key}` names {}", path.display()))
                    .context("package", folder.display().to_string()),
            );
        }
    }
    let files = match files(folder) {
        Ok(files) => files,
        Err(error) => {
            errors.push(error);
            Vec::new()
        }
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Package {
        folder: folder.to_path_buf(),
        hash: content_hash(&files),
        manifest,
        files,
    })
}

/// The packages in `packages`: each folder in it with a `package.toml`.
/// Folders without one are passed over; one that can't be loaded is
/// reported, and the others are still returned.
///
/// # Errors
///
/// None as a whole: a missing `packages` folder holds no packages. The
/// second list has each package's problems, and `package.duplicate_id` for
/// packages after the first with the same ID, which are left out.
pub fn discover(packages: &Path) -> (Vec<Package>, Vec<KaptoError>) {
    let Ok(entries) = std::fs::read_dir(packages) else {
        return (Vec::new(), Vec::new());
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join(PACKAGE_FILE).is_file())
        .collect();
    folders.sort();
    let mut found: Vec<Package> = Vec::new();
    let mut errors = Vec::new();
    for folder in folders {
        match load_package(&folder) {
            Ok(package) => {
                if let Some(first) = found
                    .iter()
                    .find(|other| other.manifest.id == package.manifest.id)
                {
                    errors.push(
                        KaptoError::new(ErrorCode::PackageDuplicateId)
                            .detail(format!(
                                "`{}` is the ID of {} and {}",
                                package.manifest.id,
                                first.folder.display(),
                                package.folder.display()
                            ))
                            .context("left out", package.folder.display().to_string()),
                    );
                } else {
                    found.push(package);
                }
            }
            Err(problems) => errors.extend(problems),
        }
    }
    (found, errors)
}

/// The content hash of `files`, which must be in byte order of their paths.
#[must_use]
pub fn content_hash(files: &[PackageFile]) -> String {
    let mut lines = String::new();
    for file in files {
        let _ = writeln!(lines, "{}  {}", file.sha256, file.path);
    }
    hex(&Sha256::digest(lines.as_bytes()))
}

/// Every file under `folder` but those named with a leading dot, hashed, in
/// byte order of their paths.
fn files(folder: &Path) -> Result<Vec<PackageFile>, KaptoError> {
    let mut found = BTreeMap::new();
    walk(folder, folder, &mut found)?;
    Ok(found.into_values().collect())
}

fn walk(
    root: &Path,
    folder: &Path,
    found: &mut BTreeMap<String, PackageFile>,
) -> Result<(), KaptoError> {
    let unreadable = |path: &Path, error: std::io::Error| {
        KaptoError::new(ErrorCode::PackageUnreadable)
            .detail(error.to_string())
            .context("path", path.display().to_string())
    };
    let entries = std::fs::read_dir(folder).map_err(|error| unreadable(folder, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| unreadable(folder, error))?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(KaptoError::new(ErrorCode::PackageUnsafePath)
                .detail("a file name isn't UTF-8".to_owned())
                .context("path", path.display().to_string()));
        };
        if name.starts_with('.') {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|error| unreadable(&path, error))?;
        if kind.is_symlink() {
            return Err(KaptoError::new(ErrorCode::PackageUnsafePath)
                .detail("a symbolic link".to_owned())
                .context("path", path.display().to_string()));
        }
        if kind.is_dir() {
            walk(root, &path, found)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let key: Vec<&str> = relative
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .collect();
            let key = key.join("/");
            let (bytes, sha256) = hash_file(&path).map_err(|error| unreadable(&path, error))?;
            found.insert(
                key.clone(),
                PackageFile {
                    path: key,
                    bytes,
                    sha256,
                },
            );
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut bytes = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    Ok((bytes, hex(&hasher.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
