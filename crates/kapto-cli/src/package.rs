// SPDX-License-Identifier: Apache-2.0

//! `kapto package`: the robot packages Kapto finds, and what identifies
//! each: its ID, version and content hash.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Subcommand, ValueEnum};
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::{Package, discover, lint, load_package};

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
