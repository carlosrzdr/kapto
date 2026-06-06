// SPDX-License-Identifier: Apache-2.0

//! Session setups saved as files in the configuration folder: profiles,
//! which say with which robot, by whom, where and how it is recorded, and
//! tasks, which say what the robot is asked to do. The two are independent:
//! an episode is recorded with a profile and a task chosen separately, so
//! operators and places change without touching the tasks, and the same
//! task is recorded by every profile.
//!
//! ```toml
//! # profiles/lab_ana.toml
//! package = "arm7"
//! operator = "Ana"
//! mode = "teleoperated"
//! ```
//!
//! ```toml
//! # tasks/pick_and_place.toml
//! name = "Pick and place"
//! prompt = "Pick up the red cube and put it in the bin"
//! subtasks = ["reach", "grasp", "lift", "place"]
//! ```
//!
//! A profile or task is known by its file's name without `.toml`. Every
//! text must say something: blank and placeholder values such as `na` are
//! refused, since they would be frozen into every episode recorded with it.

use std::fmt;
use std::path::Path;

use kapto_core::name::is_name;
use kapto_core::{ErrorCode, KaptoError};
use toml_edit::Document;

use crate::file::ConfigFile;
use crate::located::syntax_error;
use crate::reader::{FileKey, Reader};

/// The folder of profiles, in the configuration folder.
pub const PROFILES_DIR: &str = "profiles";

/// The folder of tasks, in the configuration folder.
pub const TASKS_DIR: &str = "tasks";

/// A profile using every key, for the documentation.
pub const PROFILE_EXAMPLE: &str = "package = \"arm7\"
operator = \"Ana\"
environment = \"lab 2, bench A\"
mode = \"teleoperated\"
tags = [\"shift-a\", \"new-gripper\"]
notes = \"Gripper pads replaced on Monday\"
";

/// A task using every key, for the documentation.
pub const TASK_EXAMPLE: &str = "name = \"Pick and place\"
description = \"Move the cube from the tray to the bin\"
prompt = \"Pick up the red cube and put it in the bin\"
objects = [\"red cube\", \"bin\"]
subtasks = [\"reach\", \"grasp\", \"lift\", \"place\"]
";

/// Every key of a profile, in the order they are documented.
pub const PROFILE_KEYS: [FileKey; 6] = [
    FileKey {
        name: "package",
        required: true,
        help: "The ID of the robot package recorded with it, e.g. `arm7`.",
    },
    FileKey {
        name: "operator",
        required: false,
        help: "Who operates the robot.",
    },
    FileKey {
        name: "environment",
        required: false,
        help: "Where it is recorded, e.g. `lab 2, bench A`.",
    },
    FileKey {
        name: "mode",
        required: false,
        help: "How the robot is driven: `teleoperated` (by a person), `autonomous` (by a policy), `mixed` (a policy a person takes over at times) or `scripted`.",
    },
    FileKey {
        name: "tags",
        required: false,
        help: "Words to find its episodes by later, e.g. `[\"shift-a\", \"new-gripper\"]`.",
    },
    FileKey {
        name: "notes",
        required: false,
        help: "Anything else worth knowing about the recordings, e.g. what changed on the robot.",
    },
];

/// Every key of a task, in the order they are documented.
pub const TASK_KEYS: [FileKey; 5] = [
    FileKey {
        name: "name",
        required: false,
        help: "What people call it, e.g. `Pick and place`.",
    },
    FileKey {
        name: "description",
        required: false,
        help: "A sentence or two about it.",
    },
    FileKey {
        name: "prompt",
        required: false,
        help: "The instruction given to the operator or the policy, e.g. `Pick up the red cube and put it in the bin`.",
    },
    FileKey {
        name: "objects",
        required: false,
        help: "The objects it involves, e.g. `[\"red cube\", \"bin\"]`.",
    },
    FileKey {
        name: "subtasks",
        required: false,
        help: "Its steps in order, e.g. `[\"reach\", \"grasp\", \"lift\", \"place\"]`.",
    },
];

/// How the robot is driven while it is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// By a person.
    Teleoperated,
    /// By a policy.
    Autonomous,
    /// By a policy that a person takes over at times.
    Mixed,
    /// By a fixed program.
    Scripted,
}

impl Mode {
    const ALL: [Self; 4] = [
        Self::Teleoperated,
        Self::Autonomous,
        Self::Mixed,
        Self::Scripted,
    ];

    /// The name used in files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Teleoperated => "teleoperated",
            Self::Autonomous => "autonomous",
            Self::Mixed => "mixed",
            Self::Scripted => "scripted",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A saved session setup: with which robot, by whom, where and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Its ID: the file's name without `.toml`.
    pub id: String,
    /// The robot package's ID.
    pub package: String,
    /// Who operates the robot.
    pub operator: Option<String>,
    /// Where it is recorded.
    pub environment: Option<String>,
    /// How the robot is driven.
    pub mode: Option<Mode>,
    /// Words to find its episodes by.
    pub tags: Vec<String>,
    /// Anything else worth knowing.
    pub notes: Option<String>,
}

/// What the robot is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Its ID: the file's name without `.toml`.
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

/// Reads the profile in `file`.
///
/// # Errors
///
/// Every problem found, each with the file and line: invalid TOML
/// (`config.invalid_syntax`), unknown keys (`profile.unknown_key`), a file
/// name that isn't an ID, and missing, blank or invalid values
/// (`config.invalid_value`).
pub fn parse_profile(file: &ConfigFile) -> Result<Profile, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let mut reader = Reader::new(file, document.as_table());
    let id = file_id(&mut reader, "profile");
    reader.check_keys(&PROFILE_KEYS, ErrorCode::ProfileUnknownKey, "A profile");
    let package = reader.text("package", "a robot package ID such as \"arm7\"", is_name);
    let [operator, environment, notes] =
        ["operator", "environment", "notes"].map(|key| reader.text(key, MEANINGFUL, meaningful));
    let mode = reader
        .text("mode", &modes(), |text| {
            Mode::ALL.iter().any(|mode| mode.as_str() == text)
        })
        .and_then(|text| Mode::ALL.into_iter().find(|mode| mode.as_str() == text));
    let tags = reader.texts("tags", MEANINGFUL, meaningful);
    match (id, package) {
        (Some(id), Some(package)) if reader.errors.is_empty() => Ok(Profile {
            id,
            package,
            operator,
            environment,
            mode,
            tags,
            notes,
        }),
        // Each problem was reported.
        _ => Err(reader.errors),
    }
}

/// Reads the task in `file`.
///
/// # Errors
///
/// As [`parse_profile`], with `task.unknown_key` for unknown keys.
pub fn parse_task(file: &ConfigFile) -> Result<Task, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let mut reader = Reader::new(file, document.as_table());
    let id = file_id(&mut reader, "task");
    reader.check_keys(&TASK_KEYS, ErrorCode::TaskUnknownKey, "A task");
    let [name, description, prompt] =
        ["name", "description", "prompt"].map(|key| reader.text(key, MEANINGFUL, meaningful));
    let [objects, subtasks] =
        ["objects", "subtasks"].map(|key| reader.texts(key, MEANINGFUL, meaningful));
    match id {
        Some(id) if reader.errors.is_empty() => Ok(Task {
            id,
            name,
            description,
            prompt,
            objects,
            subtasks,
        }),
        _ => Err(reader.errors),
    }
}

/// Every profile and task in a configuration folder, and every problem
/// found reading them.
#[derive(Debug, Default)]
pub struct Sessions {
    /// The profiles that could be read, by ID.
    pub profiles: Vec<Profile>,
    /// The tasks that could be read, by ID.
    pub tasks: Vec<Task>,
    /// What was wrong with the other profiles, each with its file.
    pub profile_problems: Vec<KaptoError>,
    /// What was wrong with the other tasks, each with its file.
    pub task_problems: Vec<KaptoError>,
}

/// Reads every `.toml` file in `profiles/` and `tasks/` of `config_dir`. A
/// folder that isn't there holds none; a file that can't be read or parsed
/// is left out, with its problems in [`Sessions::profile_problems`] or
/// [`Sessions::task_problems`].
#[must_use]
pub fn load_sessions(config_dir: &Path) -> Sessions {
    let mut sessions = Sessions::default();
    for file in toml_files(
        &config_dir.join(PROFILES_DIR),
        &mut sessions.profile_problems,
    ) {
        match parse_profile(&file) {
            Ok(profile) => sessions.profiles.push(profile),
            Err(errors) => sessions.profile_problems.extend(errors),
        }
    }
    for file in toml_files(&config_dir.join(TASKS_DIR), &mut sessions.task_problems) {
        match parse_task(&file) {
            Ok(task) => sessions.tasks.push(task),
            Err(errors) => sessions.task_problems.extend(errors),
        }
    }
    sessions
}

/// The `.toml` files in `folder`, by name; none if it isn't there.
fn toml_files(folder: &Path, problems: &mut Vec<KaptoError>) -> Vec<ConfigFile> {
    let unreadable = |path: &Path, error: &std::io::Error| {
        KaptoError::new(ErrorCode::ConfigUnreadable)
            .detail(error.to_string())
            .context("file", path.display().to_string())
    };
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            problems.push(unreadable(folder, &error));
            return Vec::new();
        }
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| match std::fs::read_to_string(&path) {
            Ok(contents) => Some(ConfigFile { path, contents }),
            Err(error) => {
                problems.push(unreadable(&path, &error));
                None
            }
        })
        .collect()
}

/// What a text must be.
pub(crate) const MEANINGFUL: &str =
    "text that says something: not blank, nor a placeholder such as \"na\"";

/// Whether `text` says something: not blank, nor a placeholder.
pub(crate) fn meaningful(text: &str) -> bool {
    const PLACEHOLDERS: [&str; 5] = ["na", "n/a", "none", "-", "tbd"];
    let text = text.trim();
    !text.is_empty()
        && !PLACEHOLDERS
            .iter()
            .any(|placeholder| text.eq_ignore_ascii_case(placeholder))
}

/// The ID a file gives what it holds: its name without `.toml`, if that is
/// an ID.
fn file_id(reader: &mut Reader<'_>, what: &str) -> Option<String> {
    let stem = Path::new(&reader.file.path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned();
    if is_name(&stem) {
        return Some(stem);
    }
    reader.errors.push(
        KaptoError::new(ErrorCode::ConfigInvalidValue)
            .detail(format!(
                "a {what} is known by its file's name, and `{stem}` isn't an ID: use lowercase letters, digits and underscores, starting with a letter"
            ))
            .context("file", reader.file.path.display().to_string()),
    );
    None
}

fn modes() -> String {
    let names: Vec<String> = Mode::ALL.iter().map(|mode| format!("\"{mode}\"")).collect();
    format!("one of {}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn file(path: &str, contents: &str) -> ConfigFile {
        ConfigFile {
            path: PathBuf::from(path),
            contents: contents.to_owned(),
        }
    }

    /// Each problem as `<code> <line>: <message>`.
    fn problems(errors: &[KaptoError]) -> Vec<String> {
        errors
            .iter()
            .map(|error| {
                let line = error
                    .context_entries()
                    .iter()
                    .find(|(key, _)| *key == "line")
                    .map(|(_, line)| line.clone())
                    .unwrap_or_default();
                format!("{} {line}: {}", error.code().id(), error.message())
            })
            .collect()
    }

    #[test]
    fn a_profile_needs_only_its_package_and_reads_every_key() {
        let lab = file("/config/profiles/lab_ana.toml", "package = \"arm7\"\n");
        assert_eq!(
            parse_profile(&lab).unwrap(),
            Profile {
                id: "lab_ana".to_owned(),
                package: "arm7".to_owned(),
                operator: None,
                environment: None,
                mode: None,
                tags: Vec::new(),
                notes: None,
            }
        );
        let full = file(
            "/config/profiles/lab_ana.toml",
            "package = \"arm7\"\noperator = \"Ana\"\n\
             environment = \"lab 2, bench A\"\nmode = \"teleoperated\"\n\
             tags = [\"shift-a\", \"new-gripper\"]\nnotes = \"Gripper pads replaced\"\n",
        );
        let profile = parse_profile(&full).unwrap();
        assert_eq!(profile.operator.as_deref(), Some("Ana"));
        assert_eq!(profile.environment.as_deref(), Some("lab 2, bench A"));
        assert_eq!(profile.mode, Some(Mode::Teleoperated));
        assert_eq!(profile.tags, ["shift-a", "new-gripper"]);
        assert_eq!(profile.notes.as_deref(), Some("Gripper pads replaced"));
    }

    #[test]
    fn a_task_reads_every_key_and_may_be_empty() {
        let task = file(
            "/config/tasks/pick_and_place.toml",
            "name = \"Pick and place\"\ndescription = \"Tray to bin.\"\n\
             prompt = \"Pick up the red cube and put it in the bin\"\n\
             objects = [\"red cube\", \"bin\"]\nsubtasks = [\"reach\", \"grasp\", \"lift\", \"place\"]\n",
        );
        assert_eq!(
            parse_task(&task).unwrap(),
            Task {
                id: "pick_and_place".to_owned(),
                name: Some("Pick and place".to_owned()),
                description: Some("Tray to bin.".to_owned()),
                prompt: Some("Pick up the red cube and put it in the bin".to_owned()),
                objects: vec!["red cube".to_owned(), "bin".to_owned()],
                subtasks: ["reach", "grasp", "lift", "place"]
                    .map(str::to_owned)
                    .to_vec(),
            }
        );
        let empty = parse_task(&file("/config/tasks/idle.toml", "")).unwrap();
        assert_eq!((empty.id.as_str(), empty.prompt), ("idle", None));
    }

    #[test]
    fn every_profile_and_task_in_the_folder_is_read_and_a_bad_one_reported_alone() {
        let folder = std::env::temp_dir().join(format!("kapto-{}-sessions", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join(PROFILES_DIR)).unwrap();
        let write =
            |path: &str, contents: &str| std::fs::write(folder.join(path), contents).unwrap();
        write("profiles/lab_ana.toml", PROFILE_EXAMPLE);
        write(
            "profiles/lab_ben.toml",
            "package = \"arm7\"\noperator = \"Ben\"\n",
        );
        write("profiles/broken.toml", "operator = \"na\"\n");
        write("profiles/notes.txt", "not a profile");
        let sessions = load_sessions(&folder);
        let ids: Vec<&str> = sessions
            .profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect();
        assert_eq!(ids, ["lab_ana", "lab_ben"]);
        // No tasks folder: none, and no problem.
        assert_eq!(sessions.tasks, []);
        assert_eq!(sessions.task_problems.len(), 0);
        assert_eq!(
            sessions.profile_problems.len(),
            2,
            "{:?}",
            sessions.profile_problems
        );
        assert!(sessions.profile_problems.iter().all(|problem| {
            problem
                .context_entries()
                .iter()
                .any(|(_, file)| file.ends_with("broken.toml"))
        }));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_documented_examples_use_every_key_and_are_valid() {
        let profile = file("/config/profiles/lab_ana.toml", PROFILE_EXAMPLE);
        parse_profile(&profile).unwrap();
        let task = file("/config/tasks/pick_and_place.toml", TASK_EXAMPLE);
        parse_task(&task).unwrap();
        for (example, keys) in [
            (PROFILE_EXAMPLE, &PROFILE_KEYS[..]),
            (TASK_EXAMPLE, &TASK_KEYS[..]),
        ] {
            for key in keys {
                assert!(
                    example.contains(&format!("{} = ", key.name)),
                    "{}",
                    key.name
                );
            }
        }
    }

    #[test]
    fn every_problem_in_a_profile_is_reported_with_its_line() {
        let errors = parse_profile(&file(
            "/config/profiles/lab.toml",
            // A task is chosen per episode, not by the profile.
            "task = \"pick_and_place\"\noperater = \"Ana\"\nenvironment = \"  \"\n\
             mode = \"remote\"\ntags = [\"shift-a\", \"N/A\"]\nnotes = \"na\"\n",
        ))
        .unwrap_err();
        assert_eq!(
            problems(&errors),
            [
                "profile.unknown_key 1: Unknown key in a profile: `task`",
                "profile.unknown_key 2: Unknown key in a profile: `operater` (did you mean `operator`?)",
                "config.invalid_value 1: Invalid configuration value: A profile needs `package`",
                "config.invalid_value 3: Invalid configuration value: `environment` must be text that says something: not blank, nor a placeholder such as \"na\"",
                "config.invalid_value 6: Invalid configuration value: `notes` must be text that says something: not blank, nor a placeholder such as \"na\"",
                "config.invalid_value 4: Invalid configuration value: `mode` must be one of \"teleoperated\", \"autonomous\", \"mixed\", \"scripted\"",
                "config.invalid_value 5: Invalid configuration value: `tags` must be a list of text that says something: not blank, nor a placeholder such as \"na\"",
            ]
        );
    }

    #[test]
    fn a_task_with_a_placeholder_unknown_key_or_bad_name_is_refused() {
        let errors = parse_task(&file(
            "/config/tasks/Pick Place.toml",
            "prompt = \"TBD\"\nsteps = [\"reach\"]\nsubtasks = [\"reach\", \"\"]\n",
        ))
        .unwrap_err();
        assert_eq!(
            problems(&errors),
            [
                "config.invalid_value : Invalid configuration value: a task is known by its file's name, and `Pick Place` isn't an ID: use lowercase letters, digits and underscores, starting with a letter",
                "task.unknown_key 2: Unknown key in a task: `steps`",
                "config.invalid_value 1: Invalid configuration value: `prompt` must be text that says something: not blank, nor a placeholder such as \"na\"",
                "config.invalid_value 3: Invalid configuration value: `subtasks` must be a list of text that says something: not blank, nor a placeholder such as \"na\"",
            ]
        );
        let syntax = parse_task(&file("/config/tasks/t.toml", "prompt = \n")).unwrap_err();
        assert_eq!(syntax[0].code(), ErrorCode::ConfigInvalidSyntax);
    }
}
