// SPDX-License-Identifier: Apache-2.0

//! The profiles and tasks saved in the configuration folder, as the API
//! shows them. Read again on every request, so a file saved meanwhile is
//! seen without starting serve again.
//!
//! The API's own types rather than the configuration's: the API version and
//! the file formats change on their own schedules.

use std::path::Path;

use kapto_capture::control::ChosenProfile;
use kapto_config::{Mode, PROFILES_DIR, Profile, TASKS_DIR, Task, load_sessions};
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::episode::content_hash;
use kapto_store::{OperationMode, SessionProfile, SessionTask};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::ApiError;

/// The saved profiles, and which can be used with the robot package Kapto
/// runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProfilesOverview {
    /// The robot package Kapto runs, if its streams belong to one.
    pub package: Option<String>,
    /// Every profile that could be read, by ID.
    pub profiles: Vec<ProfileSummary>,
    /// What is wrong with the profiles that couldn't be.
    pub problems: Vec<ApiError>,
}

/// One saved profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProfileSummary {
    /// Its ID: its file's name without `.toml`.
    pub id: String,
    /// The robot package it records.
    pub package: String,
    /// Whether it can be used now: its package is the one Kapto runs. One
    /// that isn't needs Kapto started with that package.
    pub available: bool,
    /// Who operates the robot.
    pub operator: Option<String>,
    /// Where it is recorded.
    pub environment: Option<String>,
    /// How the robot is driven: `teleoperated`, `autonomous`, `mixed` or
    /// `scripted`.
    pub mode: Option<String>,
    /// Words to find its episodes by.
    pub tags: Vec<String>,
    /// Anything else worth knowing.
    pub notes: Option<String>,
}

/// The saved tasks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TasksOverview {
    /// Every task that could be read, by ID.
    pub tasks: Vec<TaskSummary>,
    /// What is wrong with the tasks that couldn't be.
    pub problems: Vec<ApiError>,
}

/// One saved task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TaskSummary {
    /// Its ID: its file's name without `.toml`.
    pub id: String,
    /// What people call it.
    pub name: Option<String>,
    /// A sentence or two about it.
    pub description: Option<String>,
    /// The instruction given to the operator or the policy.
    pub prompt: Option<String>,
    /// The objects it involves.
    pub objects: Vec<String>,
    /// Its steps in order.
    pub subtasks: Vec<String>,
}

/// The profiles in `config_folder`, each available if it records `package`.
#[must_use]
pub fn profiles(config_folder: &Path, package: Option<&str>) -> ProfilesOverview {
    let sessions = load_sessions(config_folder);
    ProfilesOverview {
        package: package.map(str::to_owned),
        profiles: sessions
            .profiles
            .into_iter()
            .map(|profile| summary(profile, package))
            .collect(),
        problems: sessions
            .profile_problems
            .iter()
            .map(ApiError::from)
            .collect(),
    }
}

/// The tasks in `config_folder`.
#[must_use]
pub fn tasks(config_folder: &Path) -> TasksOverview {
    let sessions = load_sessions(config_folder);
    TasksOverview {
        tasks: sessions
            .tasks
            .into_iter()
            .map(|task: Task| TaskSummary {
                id: task.id,
                name: task.name,
                description: task.description,
                prompt: task.prompt,
                objects: task.objects,
                subtasks: task.subtasks,
            })
            .collect(),
        problems: sessions.task_problems.iter().map(ApiError::from).collect(),
    }
}

/// The profile `profile` and the task `task` of `config_folder`, read now,
/// as an episode is started with them: either may be left out.
///
/// # Errors
///
/// `profile.not_found` or `task.not_found` if there is no such file; the
/// file's own problems if it is there but can't be read.
pub fn resolve(
    config_folder: &Path,
    profile: Option<&str>,
    task: Option<&str>,
) -> Result<(Option<ChosenProfile>, Option<SessionTask>), KaptoError> {
    if profile.is_none() && task.is_none() {
        return Ok((None, None));
    }
    let sessions = load_sessions(config_folder);
    let chosen = match profile {
        None => None,
        Some(wanted) => match sessions
            .profiles
            .iter()
            .find(|profile| profile.id == wanted)
        {
            Some(profile) => Some(ChosenProfile {
                package: profile.package.clone(),
                profile: kept_profile(profile),
            }),
            None => {
                return Err(problem_of(sessions.profile_problems, PROFILES_DIR, wanted)
                    .unwrap_or_else(|| not_found(ErrorCode::ProfileNotFound, wanted)));
            }
        },
    };
    let task = match task {
        None => None,
        Some(wanted) => match sessions.tasks.iter().find(|task| task.id == wanted) {
            Some(task) => Some(kept_task(task)),
            None => {
                return Err(problem_of(sessions.task_problems, TASKS_DIR, wanted)
                    .unwrap_or_else(|| not_found(ErrorCode::TaskNotFound, wanted)));
            }
        },
    };
    Ok((chosen, task))
}

fn not_found(code: ErrorCode, id: &str) -> KaptoError {
    KaptoError::new(code).detail(format!("`{id}`"))
}

/// The first problem of the file `<folder>/<id>.toml`, if it has one: it is
/// there, but can't be read.
fn problem_of(problems: Vec<KaptoError>, folder: &str, id: &str) -> Option<KaptoError> {
    let file = Path::new(folder).join(format!("{id}.toml"));
    problems.into_iter().find(|problem| {
        problem
            .context_entries()
            .iter()
            .any(|(key, value)| *key == "file" && Path::new(value).ends_with(&file))
    })
}

fn kept_profile(profile: &Profile) -> SessionProfile {
    let mut kept = SessionProfile {
        id: profile.id.clone(),
        hash: None,
        operator: profile.operator.clone(),
        environment: profile.environment.clone(),
        mode: profile.mode.map(|mode| match mode {
            Mode::Teleoperated => OperationMode::Teleoperated,
            Mode::Autonomous => OperationMode::Autonomous,
            Mode::Mixed => OperationMode::Mixed,
            Mode::Scripted => OperationMode::Scripted,
        }),
        tags: profile.tags.clone(),
        notes: profile.notes.clone(),
    };
    kept.hash = Some(content_hash(&kept));
    kept
}

fn kept_task(task: &Task) -> SessionTask {
    let mut kept = SessionTask {
        id: task.id.clone(),
        hash: None,
        name: task.name.clone(),
        description: task.description.clone(),
        prompt: task.prompt.clone(),
        objects: task.objects.clone(),
        subtasks: task.subtasks.clone(),
    };
    kept.hash = Some(content_hash(&kept));
    kept
}

fn summary(profile: Profile, running: Option<&str>) -> ProfileSummary {
    ProfileSummary {
        available: running == Some(profile.package.as_str()),
        id: profile.id,
        package: profile.package,
        operator: profile.operator,
        environment: profile.environment,
        mode: profile.mode.map(|mode| mode.to_string()),
        tags: profile.tags,
        notes: profile.notes,
    }
}
