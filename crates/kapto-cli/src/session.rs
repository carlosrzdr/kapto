// SPDX-License-Identifier: Apache-2.0

//! `kapto profile` and `kapto task`: the profiles and tasks saved in the
//! configuration folder, and every problem with them.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use kapto_config::{PROFILES_DIR, Profile, Sessions, Task, load_sessions};
use kapto_core::{ErrorCode, KaptoError, say};
use kapto_robot::discover;

use crate::SettingFlags;
use crate::output::{out, outln};

#[derive(Subcommand)]
pub(crate) enum ProfileCommand {
    /// List the profiles in the configuration folder: each one's ID, robot
    /// package, mode and operator.
    List,
    /// Show one profile: everything it says.
    Show {
        /// The profile's ID: its file's name without .toml.
        profile: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum TaskCommand {
    /// List the tasks in the configuration folder: each one's ID, name and
    /// number of subtasks.
    List,
    /// Show one task: everything it says.
    Show {
        /// The task's ID: its file's name without .toml.
        task: String,
    },
}

pub(crate) fn profile(flags: &SettingFlags, command: &ProfileCommand) -> ExitCode {
    finish(load(flags).and_then(|(sessions, folder)| match command {
        ProfileCommand::List => {
            let rows = sessions.profiles.iter().map(|profile| {
                [
                    profile.id.clone(),
                    profile.package.clone(),
                    profile
                        .mode
                        .map_or_else(String::new, |mode| mode.to_string()),
                    profile.operator.clone().unwrap_or_default(),
                ]
            });
            list(
                &folder,
                "profiles",
                ["ID", "PACKAGE", "MODE", "OPERATOR"],
                rows,
            );
            everything_valid(sessions.profile_problems)
        }
        ProfileCommand::Show { profile } => {
            let found = find(
                &sessions.profiles,
                profile,
                |p| &p.id,
                ErrorCode::ProfileNotFound,
            )?;
            out!("{}", describe_profile(found));
            Ok(())
        }
    }))
}

pub(crate) fn task(flags: &SettingFlags, command: &TaskCommand) -> ExitCode {
    finish(load(flags).and_then(|(sessions, folder)| match command {
        TaskCommand::List => {
            let rows = sessions.tasks.iter().map(|task| {
                [
                    task.id.clone(),
                    task.name.clone().unwrap_or_default(),
                    task.subtasks.len().to_string(),
                ]
            });
            list(&folder, "tasks", ["ID", "NAME", "SUBTASKS"], rows);
            everything_valid(sessions.task_problems)
        }
        TaskCommand::Show { task } => {
            let found = find(&sessions.tasks, task, |t| &t.id, ErrorCode::TaskNotFound)?;
            out!("{}", describe_task(found));
            Ok(())
        }
    }))
}

/// The profiles and tasks in the configuration folder, and that folder.
/// Besides the problems reading them, each profile whose robot package
/// isn't among the packages (`paths.packages`) is a problem.
///
/// # Errors
///
/// The settings' problems, which come first.
pub(crate) fn load(flags: &SettingFlags) -> Result<(Sessions, PathBuf), Vec<KaptoError>> {
    let (_, deployment) = crate::config::load(flags)?;
    let folder = crate::config::config_folder(flags);
    let mut sessions = load_sessions(&folder);
    let packages_dir = &deployment.packages_dir.value;
    let (packages, _) = discover(packages_dir);
    for profile in &sessions.profiles {
        if !packages
            .iter()
            .any(|package| package.manifest.id == profile.package)
        {
            sessions.profile_problems.push(
                KaptoError::new(ErrorCode::ProfileUnknownPackage)
                    .detail(format!(
                        "profile `{}` records robot package `{}`, which isn't in {}",
                        profile.id,
                        profile.package,
                        packages_dir.display()
                    ))
                    .context(
                        "file",
                        folder
                            .join(PROFILES_DIR)
                            .join(format!("{}.toml", profile.id))
                            .display()
                            .to_string(),
                    ),
            );
        }
    }
    Ok((sessions, folder))
}

/// Prints `rows` under `header`, columns aligned, or that there are none.
fn list<const N: usize>(
    folder: &Path,
    what: &str,
    header: [&str; N],
    rows: impl Iterator<Item = [String; N]>,
) {
    let rows: Vec<[String; N]> = rows.collect();
    if rows.is_empty() {
        outln!("No {what} in {}.", folder.join(what).display());
        return;
    }
    let header = header.map(str::to_owned);
    let widths: Vec<usize> = (0..N)
        .map(|column| {
            rows.iter()
                .chain([&header])
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        outln!("{}", cells.join("  ").trim_end());
    }
}

/// The one with ID `wanted`, or an error naming the ones there are.
fn find<'a, T>(
    all: &'a [T],
    wanted: &str,
    id: fn(&T) -> &String,
    missing: ErrorCode,
) -> Result<&'a T, Vec<KaptoError>> {
    all.iter().find(|one| id(one) == wanted).ok_or_else(|| {
        let known: Vec<&str> = all.iter().map(|one| id(one).as_str()).collect();
        let detail = if known.is_empty() {
            format!("`{wanted}`; there are none")
        } else {
            format!("`{wanted}`; there are: {}", known.join(", "))
        };
        vec![KaptoError::new(missing).detail(detail)]
    })
}

fn describe_profile(profile: &Profile) -> String {
    let mut lines = vec![
        format!("Profile      {}", profile.id),
        format!("Package      {}", profile.package),
    ];
    let optional = [
        ("Operator", profile.operator.clone()),
        ("Environment", profile.environment.clone()),
        ("Mode", profile.mode.map(|mode| mode.to_string())),
        (
            "Tags",
            (!profile.tags.is_empty()).then(|| profile.tags.join(", ")),
        ),
        ("Notes", profile.notes.clone()),
    ];
    lines.extend(
        optional
            .into_iter()
            .filter_map(|(label, value)| value.map(|value| format!("{label:<12} {value}"))),
    );
    lines.join("\n") + "\n"
}

fn describe_task(task: &Task) -> String {
    let mut lines = vec![format!("Task         {}", task.id)];
    let optional = [
        ("Name", task.name.clone()),
        ("Description", task.description.clone()),
        ("Prompt", task.prompt.clone()),
        (
            "Objects",
            (!task.objects.is_empty()).then(|| task.objects.join(", ")),
        ),
        (
            "Subtasks",
            (!task.subtasks.is_empty()).then(|| task.subtasks.join(" → ")),
        ),
    ];
    lines.extend(
        optional
            .into_iter()
            .filter_map(|(label, value)| value.map(|value| format!("{label:<12} {value}"))),
    );
    lines.join("\n") + "\n"
}

fn everything_valid(problems: Vec<KaptoError>) -> Result<(), Vec<KaptoError>> {
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn finish(result: Result<(), Vec<KaptoError>>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            say!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}
