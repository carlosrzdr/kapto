// SPDX-License-Identifier: Apache-2.0

//! `kapto package`: the robot packages Kapto finds, and what identifies
//! each: its ID, version and content hash.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Subcommand, ValueEnum};
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::{Package, discover, import_package, lint, load_package};

use crate::SettingFlags;

#[derive(Subcommand)]
pub(crate) enum PackageCommand {
    /// List the robot packages in the packages folder (`paths.packages`):
    /// each one's ID, version and content hash.
    List,
    /// Show one robot package: what its package.toml says, its files and
    /// its content hash.
    Show {
        /// A package folder, or the ID of a package in the packages folder.
        package: String,
    },
    /// Make a robot package from a robot description found elsewhere, such
    /// as a vendor's repository: the URDF and every mesh and texture it
    /// names, copied into a new package in the packages folder so each is
    /// found as the URDF names it, with a package.toml, a streams.toml to
    /// fill in and a mapping.toml listing each moving joint.
    Import {
        /// The robot's URDF.
        urdf: PathBuf,
        /// The new package's ID, which is also its folder's name.
        #[arg(long)]
        id: String,
        /// What people call the robot; the URDF's robot name if not given.
        #[arg(long)]
        name: Option<String>,
        /// An argument of a xacro description, `name:=value`, as ROS's
        /// xacro takes it; repeat for each.
        #[arg(long = "arg", value_name = "NAME:=VALUE")]
        args: Vec<String>,
    },
    /// Check a robot package: its files, the robot description and meshes,
    /// the streams and the mapping, reporting every problem with where it is
    /// and what to do. Fails if there are errors; warnings don't.
    Lint {
        /// A package folder, or the ID of a package in the packages folder.
        package: String,
        /// text for people, json for tools.
        #[arg(long, value_enum, default_value_t = LintFormat::Text)]
        format: LintFormat,
    },
}

/// How `kapto package lint` reports.
#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum LintFormat {
    Text,
    Json,
}

pub(crate) fn package(flags: &SettingFlags, command: &PackageCommand) -> ExitCode {
    let result = match command {
        PackageCommand::List => list(flags),
        PackageCommand::Show { package } => show(flags, package),
        PackageCommand::Lint { package, format } => return lint_package(flags, package, *format),
        PackageCommand::Import {
            urdf,
            id,
            name,
            args,
        } => import(flags, urdf, id, name.as_deref(), args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

/// Lists every package found; any that couldn't be loaded are reported
/// after, and fail the command.
fn list(flags: &SettingFlags) -> Result<(), Vec<KaptoError>> {
    let (_, deployment) = crate::config::load(flags)?;
    let folder = &deployment.packages_dir.value;
    let (packages, errors) = discover(folder);
    if packages.is_empty() && errors.is_empty() {
        println!("No robot packages in {}.", folder.display());
        return Ok(());
    }
    let rows: Vec<[String; 4]> = packages
        .iter()
        .map(|package| {
            [
                package.manifest.id.clone(),
                package.manifest.version.clone(),
                package.hash[..12].to_owned(),
                package.folder.display().to_string(),
            ]
        })
        .collect();
    let header = ["ID", "VERSION", "HASH", "FOLDER"].map(str::to_owned);
    let widths: Vec<usize> = (0..3)
        .map(|column| {
            rows.iter()
                .chain([&header])
                .map(|row| row[column].len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2]
        );
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Shows the package in a folder, or with an ID in the packages folder.
fn show(flags: &SettingFlags, wanted: &str) -> Result<(), Vec<KaptoError>> {
    let (folder, _) = find(flags, wanted)?;
    print!("{}", describe(&load_package(&folder)?));
    Ok(())
}

/// Imports the robot description at `urdf`, URDF or xacro expanded with
/// `args`, as the package `id` in the packages folder, saying what was
/// copied, what wasn't and why, and what is left to do.
fn import(
    flags: &SettingFlags,
    urdf: &Path,
    id: &str,
    name: Option<&str>,
    args: &[String],
) -> Result<(), Vec<KaptoError>> {
    let args = args
        .iter()
        .map(|arg| {
            arg.split_once(":=")
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .ok_or_else(|| {
                    vec![
                        KaptoError::new(ErrorCode::ConfigInvalidValue)
                            .detail(format!("`--arg {arg}`: an argument is `name:=value`")),
                    ]
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let (_, deployment) = crate::config::load(flags)?;
    let packages = deployment.packages_dir.value;
    // `package://` paths are looked up among the packages, as when Kapto runs.
    let imported = import_package(
        urdf,
        &args,
        std::slice::from_ref(&packages),
        &packages.join(id),
        id,
        name,
    )?;
    let bytes: u64 = imported.copied.iter().map(|(_, bytes)| bytes).sum();
    println!(
        "Imported {} as the package `{id}` in {}: {} and {} files beside it ({}).",
        urdf.display(),
        imported.folder.display(),
        imported.urdf.display(),
        imported.copied.len(),
        crate::upload::mebibytes(bytes),
    );
    if !imported.left_out.is_empty() {
        println!(
            "\nLeft out, {} of what it names:\n",
            imported.left_out.len()
        );
        let reports: Vec<String> = imported.left_out.iter().map(KaptoError::report).collect();
        println!("{}", reports.join("\n\n"));
    }
    println!(
        "\nNext: declare what the robot sends in {folder}/streams.toml, give each of its {} moving joints the field that drives it in {folder}/mapping.toml, then check it with `kapto package lint {id}`.",
        imported.joints.len(),
        folder = imported.folder.display(),
    );
    Ok(())
}

/// Lints the package in a folder, or with an ID in the packages folder.
fn lint_package(flags: &SettingFlags, wanted: &str, format: LintFormat) -> ExitCode {
    let (folder, packages) = match find(flags, wanted) {
        Ok(found) => found,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            return ExitCode::FAILURE;
        }
    };
    // `package://` paths are looked up among the packages, and next to
    // this one.
    let mut search = vec![packages];
    search.extend(folder.parent().map(Path::to_path_buf));
    let report = lint(&folder, &search);
    match format {
        LintFormat::Text => print!("{}", report.to_text()),
        LintFormat::Json => println!("{}", report.to_json()),
    }
    if report.errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The folder of the package `wanted` names, a folder or an ID in the
/// packages folder, and the packages folder.
pub(crate) fn find(
    flags: &SettingFlags,
    wanted: &str,
) -> Result<(PathBuf, PathBuf), Vec<KaptoError>> {
    let (_, deployment) = crate::config::load(flags)?;
    let packages = deployment.packages_dir.value;
    let folder = Path::new(wanted);
    if folder.is_dir() {
        return Ok((folder.to_path_buf(), packages));
    }
    let (found, _) = discover(&packages);
    let package = found
        .into_iter()
        .find(|package| package.manifest.id == wanted)
        .ok_or_else(|| {
            vec![KaptoError::new(ErrorCode::PackageNotFound).detail(format!(
                "`{wanted}` is neither a folder nor the ID of a package in {}",
                packages.display()
            ))]
        })?;
    Ok((package.folder, packages))
}

/// A package as `kapto package show` prints it.
fn describe(package: &Package) -> String {
    let manifest = &package.manifest;
    let mut text = format!("{} {}", manifest.id, manifest.version);
    if let Some(name) = &manifest.name {
        let _ = write!(text, ": {name}");
    }
    if let Some(vendor) = &manifest.vendor {
        let _ = write!(text, " ({vendor})");
    }
    text.push('\n');
    if let Some(description) = &manifest.description {
        let _ = writeln!(text, "{description}");
    }
    let _ = write!(
        text,
        "\nFolder:   {}\nHash:     {}\nRobot:    {}\nStreams:  {}\nMapping:  {}\n",
        package.folder.display(),
        package.hash,
        manifest.urdf.display(),
        manifest.streams.display(),
        manifest.mapping.display()
    );
    if let Some(contact) = &manifest.contact {
        let _ = writeln!(text, "Contact:  {contact}");
    }
    text.push_str("\nFiles (SHA-256, bytes, path):\n");
    for file in &package.files {
        let _ = writeln!(text, "  {}  {:>9}  {}", file.sha256, file.bytes, file.path);
    }
    text
}
