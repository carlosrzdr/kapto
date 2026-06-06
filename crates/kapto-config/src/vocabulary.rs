// SPDX-License-Identifier: Apache-2.0

//! The kinds of events people and programs may send, saved in the
//! configuration folder as `events.toml`, beside profiles and tasks: what
//! each is called, its colour, and whether it is usually a stretch rather
//! than a moment. Each task's subtasks are kinds too (`subtask.<name>`),
//! stretches by nature. Without the file, a few kinds do for a start.
//!
//! ```toml
//! # events.toml
//! [kinds.grasp]
//! name = "Grasp"
//! color = "#4c9be8"
//! stretch = true
//! ```
//!
//! Kinds beginning `kapto.` are Kapto's own, and `subtask.` the tasks'.

use std::path::Path;

use kapto_core::name::{is_dotted_name, is_name};
use kapto_core::{ErrorCode, KaptoError};
use toml_edit::Document;

use crate::file::ConfigFile;
use crate::located::syntax_error;
use crate::reader::{FileKey, Reader};
use crate::session::{MEANINGFUL, Task, meaningful};

/// The vocabulary's file, in the configuration folder.
pub const EVENTS_FILE: &str = "events.toml";

/// An example of the file, as the reference shows it.
pub const EVENTS_EXAMPLE: &str = "[kinds.grasp]
name = \"Grasp\"
color = \"#4c9be8\"
stretch = true
description = \"The gripper closing on an object and holding it.\"

[kinds.slip]
name = \"Slip\"
color = \"#e86a5a\"
";

/// Every key of a kind, in the order they are documented.
pub const EVENT_KIND_KEYS: [FileKey; 4] = [
    FileKey {
        name: "name",
        required: false,
        help: "What people call it, e.g. `Grasp`; its kind if not given.",
    },
    FileKey {
        name: "color",
        required: false,
        help: "Its colour on the timeline, as `#rrggbb`.",
    },
    FileKey {
        name: "stretch",
        required: false,
        help: "Whether it is usually a stretch, begun and ended, rather than a moment: how the page offers it. `false` if not given.",
    },
    FileKey {
        name: "description",
        required: false,
        help: "A sentence about when to send it.",
    },
];

/// The kinds there are without `events.toml`: `(kind, name, stretch)`.
pub const DEFAULT_KINDS: [(&str, &str, bool); 3] = [
    ("note", "Note", false),
    ("problem", "Problem", false),
    ("intervention", "Intervention", true),
];

/// The prefix of a task's subtasks' kinds.
pub const SUBTASK_KINDS: &str = "subtask.";

/// One kind of event that may be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventKind {
    /// The kind, as events carry it, e.g. `grasp` or `subtask.reach`.
    pub kind: String,
    /// What people call it.
    pub name: String,
    /// Its colour, `#rrggbb`, if given.
    pub color: Option<String>,
    /// Whether it is usually a stretch.
    pub stretch: bool,
    /// When to send it, if said.
    pub description: Option<String>,
    /// Where it was defined.
    pub from: KindSource,
}

/// Where a kind of event was defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindSource {
    /// `events.toml`.
    File,
    /// Kapto's defaults, there without `events.toml`.
    Default,
    /// A subtask of the task with this ID.
    Task(String),
}

/// Every kind of event that may be sent, and what was wrong with the file.
#[derive(Debug, Default)]
pub struct Vocabulary {
    /// The kinds, the file's or the defaults first, then each task's
    /// subtasks.
    pub kinds: Vec<EventKind>,
    /// What was wrong with `events.toml`; its kinds are left out then, and
    /// the defaults used instead.
    pub problems: Vec<KaptoError>,
}

impl Vocabulary {
    /// The kind `kind`, if it may be sent.
    #[must_use]
    pub fn kind(&self, kind: &str) -> Option<&EventKind> {
        self.kinds.iter().find(|known| known.kind == kind)
    }
}

/// Reads the vocabulary in `file`.
///
/// # Errors
///
/// Every problem found, each with the file and line: invalid TOML
/// (`config.invalid_syntax`), unknown keys (`events.unknown_key`), and
/// kinds or values that aren't as they must be (`config.invalid_value`).
pub fn parse_vocabulary(file: &ConfigFile) -> Result<Vec<EventKind>, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let mut top = Reader::new(file, document.as_table());
    top.check_keys(
        &[FileKey {
            name: "kinds",
            required: false,
            help: "",
        }],
        ErrorCode::EventsUnknownKey,
        "The file",
    );
    let mut errors = std::mem::take(&mut top.errors);
    let mut kinds = Vec::new();
    let Some(table) = document.get("kinds") else {
        return if errors.is_empty() {
            Ok(kinds)
        } else {
            Err(errors)
        };
    };
    let Some(table) = table.as_table_like() else {
        let mut top = Reader::new(file, document.as_table());
        top.located(
            KaptoError::new(ErrorCode::ConfigInvalidValue)
                .detail("`kinds` must be a table of kinds, e.g. `[kinds.grasp]`".to_owned()),
            "kinds",
        );
        errors.extend(top.errors);
        return Err(errors);
    };
    for (kind, item) in table.iter() {
        let mut outer = Reader::new(file, table);
        let own = kind.starts_with("kapto.") || kind.starts_with(SUBTASK_KINDS);
        if !is_dotted_name(kind) || own {
            outer.located(
                KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                    "the kind `{kind}` must be lowercase words joined by dots, not beginning `kapto.` (Kapto's own) or `{SUBTASK_KINDS}` (the tasks')"
                )),
                kind,
            );
            errors.extend(outer.errors);
            continue;
        }
        let Some(fields) = item.as_table_like() else {
            outer.located(
                KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                    "`kinds.{kind}` must be a table, e.g. `[kinds.{kind}]`"
                )),
                kind,
            );
            errors.extend(outer.errors);
            continue;
        };
        let mut reader = Reader::new(file, fields);
        reader.check_keys(&EVENT_KIND_KEYS, ErrorCode::EventsUnknownKey, "A kind");
        let [name, description] =
            ["name", "description"].map(|key| reader.text(key, MEANINGFUL, meaningful));
        let color = reader.text("color", "a colour as `#rrggbb`", is_color);
        let stretch = reader.switch("stretch").unwrap_or(false);
        if reader.errors.is_empty() {
            kinds.push(EventKind {
                kind: kind.to_owned(),
                name: name.unwrap_or_else(|| kind.to_owned()),
                color,
                stretch,
                description,
                from: KindSource::File,
            });
        }
        errors.extend(reader.errors);
    }
    if errors.is_empty() {
        Ok(kinds)
    } else {
        Err(errors)
    }
}

/// Every kind of event that may be sent with the configuration in
/// `config_dir` and its `tasks`: those of `events.toml` (or the defaults
/// without it, or with problems), then each task's subtasks.
#[must_use]
pub fn load_vocabulary(config_dir: &Path, tasks: &[Task]) -> Vocabulary {
    let path = config_dir.join(EVENTS_FILE);
    let mut vocabulary = Vocabulary::default();
    let from_file = match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let file = ConfigFile { path, contents };
            match parse_vocabulary(&file) {
                Ok(kinds) => Some(kinds),
                Err(problems) => {
                    vocabulary.problems = problems;
                    None
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            vocabulary.problems.push(
                KaptoError::new(ErrorCode::ConfigUnreadable)
                    .detail(error.to_string())
                    .context("file", path.display().to_string()),
            );
            None
        }
    };
    vocabulary.kinds = from_file.unwrap_or_else(|| {
        DEFAULT_KINDS
            .iter()
            .map(|&(kind, name, stretch)| EventKind {
                kind: kind.to_owned(),
                name: name.to_owned(),
                color: None,
                stretch,
                description: None,
                from: KindSource::Default,
            })
            .collect()
    });
    for task in tasks {
        for subtask in &task.subtasks {
            let Some(word) = subtask_word(subtask) else {
                continue;
            };
            let kind = format!("{SUBTASK_KINDS}{word}");
            if vocabulary.kind(&kind).is_none() {
                vocabulary.kinds.push(EventKind {
                    kind,
                    name: subtask.clone(),
                    color: None,
                    stretch: true,
                    description: None,
                    from: KindSource::Task(task.id.clone()),
                });
            }
        }
    }
    vocabulary
}

/// A subtask as a word of a kind: lowercase, with anything but letters and
/// digits as underscores (`Pick up` → `pick_up`); `None` if no word is left
/// that starts with a letter.
fn subtask_word(subtask: &str) -> Option<String> {
    let mut word = String::new();
    for c in subtask.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            word.push(c);
        } else if !word.ends_with('_') && !word.is_empty() {
            word.push('_');
        }
    }
    let word = word.trim_end_matches('_').to_owned();
    is_name(&word).then_some(word)
}

/// Whether `text` is a colour as `#rrggbb`.
fn is_color(text: &str) -> bool {
    text.len() == 7 && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn file(contents: &str) -> ConfigFile {
        ConfigFile {
            contents: contents.to_owned(),
            path: PathBuf::from("/config/events.toml"),
        }
    }

    #[test]
    fn the_example_reads_as_its_kinds() {
        let kinds = parse_vocabulary(&file(EVENTS_EXAMPLE)).unwrap();
        let read: Vec<(&str, &str, Option<&str>, bool)> = kinds
            .iter()
            .map(|kind| {
                (
                    kind.kind.as_str(),
                    kind.name.as_str(),
                    kind.color.as_deref(),
                    kind.stretch,
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("grasp", "Grasp", Some("#4c9be8"), true),
                ("slip", "Slip", Some("#e86a5a"), false)
            ]
        );
    }

    #[test]
    fn every_problem_is_reported_with_its_line() {
        let errors = parse_vocabulary(&file(
            "colours = 1\n[kinds.grasp]\ncolor = \"blue\"\nstrech = true\n[kinds.\"kapto.mine\"]\n[kinds.\"subtask.reach\"]\n",
        ))
        .unwrap_err();
        let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
        let codes: Vec<ErrorCode> = errors.iter().map(KaptoError::code).collect();
        assert_eq!(
            codes,
            [
                ErrorCode::EventsUnknownKey,
                ErrorCode::EventsUnknownKey,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
                ErrorCode::ConfigInvalidValue,
            ],
            "{reports:#?}"
        );
        assert!(
            reports[1].contains("stretch"),
            "a suggestion: {}",
            reports[1]
        );
        assert!(reports[2].contains("#rrggbb"), "{}", reports[2]);
        assert!(reports[3].contains("Kapto's own"), "{}", reports[3]);
    }

    #[test]
    fn without_the_file_the_defaults_and_every_task_s_subtasks_may_be_sent() {
        let folder = std::env::temp_dir().join(format!("kapto-vocabulary-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let task = Task {
            id: "pick_and_place".to_owned(),
            name: None,
            description: None,
            prompt: None,
            objects: Vec::new(),
            subtasks: vec!["Reach".to_owned(), "Pick up!".to_owned(), "42".to_owned()],
        };
        let vocabulary = load_vocabulary(&folder, &[task.clone(), task]);
        let kinds: Vec<&str> = vocabulary
            .kinds
            .iter()
            .map(|kind| kind.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                "note",
                "problem",
                "intervention",
                "subtask.reach",
                "subtask.pick_up"
            ]
        );
        let reach = vocabulary.kind("subtask.reach").unwrap();
        assert_eq!((reach.name.as_str(), reach.stretch), ("Reach", true));
        assert_eq!(reach.from, KindSource::Task("pick_and_place".to_owned()));
        assert!(vocabulary.problems.is_empty());

        // A file with a problem: reported, and the defaults stand in for it.
        std::fs::write(folder.join(EVENTS_FILE), "[kinds.Grasp]\n").unwrap();
        let vocabulary = load_vocabulary(&folder, &[]);
        assert_eq!(vocabulary.problems.len(), 1);
        assert_eq!(vocabulary.kinds.len(), DEFAULT_KINDS.len());
        std::fs::write(folder.join(EVENTS_FILE), EVENTS_EXAMPLE).unwrap();
        assert_eq!(
            load_vocabulary(&folder, &[]).kinds.len(),
            2,
            "the file's kinds only"
        );
        std::fs::remove_dir_all(folder).unwrap();
    }
}
