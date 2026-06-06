// SPDX-License-Identifier: Apache-2.0

//! `kapto profile` and `kapto task`, and `kapto config validate` checking
//! them: run as a real process on a configuration folder of the test's own,
//! with the sample arm as the only robot package.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::root;
use kapto_testkit::scratch;

/// A configuration folder with `kapto.toml`, `profiles/` and `tasks/`.
fn folder(test: &str, files: &[(&str, &str)]) -> PathBuf {
    let folder = scratch(test);
    std::fs::write(folder.join("kapto.toml"), "").unwrap();
    for (path, contents) in files {
        let path = folder.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    folder
}

/// Runs `kapto` with the configuration in `folder` and the repository's
/// packages.
fn kapto(folder: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(args)
        .arg("--config")
        .arg(folder.join("kapto.toml"))
        .arg("--packages-dir")
        .arg(root().join("packages"))
        .env_clear()
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

const ANA: (&str, &str) = (
    "profiles/lab_ana.toml",
    "package = \"arm7\"\noperator = \"Ana\"\nmode = \"teleoperated\"\ntags = [\"shift-a\"]\n",
);
const NIGHT: (&str, &str) = (
    "profiles/night.toml",
    "package = \"arm7\"\nmode = \"autonomous\"\n",
);
const PICK: (&str, &str) = (
    "tasks/pick_and_place.toml",
    "name = \"Pick and place\"\nprompt = \"Pick up the red cube\"\n\
     subtasks = [\"reach\", \"grasp\", \"lift\", \"place\"]\n",
);

#[test]
fn profiles_and_tasks_are_listed_and_shown_from_the_configuration_folder() {
    let folder = folder("sessions-listed", &[ANA, NIGHT, PICK]);
    let listed = kapto(&folder, &["profile", "list"]);
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    assert_eq!(
        text(&listed.stdout),
        "ID       PACKAGE  MODE          OPERATOR\n\
         lab_ana  arm7     teleoperated  Ana\n\
         night    arm7     autonomous\n"
    );
    assert_eq!(
        text(&kapto(&folder, &["task", "list"]).stdout),
        "ID              NAME            SUBTASKS\npick_and_place  Pick and place  4\n"
    );
    assert_eq!(
        text(&kapto(&folder, &["profile", "show", "lab_ana"]).stdout),
        "Profile      lab_ana\nPackage      arm7\nOperator     Ana\nMode         teleoperated\nTags         shift-a\n"
    );
    assert_eq!(
        text(&kapto(&folder, &["task", "show", "pick_and_place"]).stdout),
        "Task         pick_and_place\nName         Pick and place\nPrompt       Pick up the red cube\n\
         Subtasks     reach → grasp → lift → place\n"
    );
    let valid = kapto(&folder, &["config", "validate"]);
    assert!(valid.status.success(), "{}", text(&valid.stderr));
    assert!(text(&valid.stdout).contains("Profiles: 2, tasks: 1, in "));
}

#[test]
fn none_saved_is_said_and_is_not_a_problem() {
    let folder = folder("sessions-none", &[]);
    let listed = kapto(&folder, &["profile", "list"]);
    assert!(listed.status.success());
    assert!(text(&listed.stdout).starts_with("No profiles in "));
    let missing = kapto(&folder, &["task", "show", "stacking"]);
    assert!(!missing.status.success());
    assert!(
        text(&missing.stderr)
            .contains("error[task.not_found]: No such task: `stacking`; there are none")
    );
}

#[test]
fn a_profile_for_a_package_that_isnt_there_fails_its_list_and_validate_but_not_the_tasks() {
    let cobot = ("profiles/cobot.toml", "package = \"cobot\"\n");
    let bad_task = ("tasks/broken.toml", "prompt = \"na\"\n");
    let folder = folder("sessions-problems", &[ANA, cobot, PICK]);

    let listed = kapto(&folder, &["profile", "list"]);
    assert!(!listed.status.success());
    // The others are still listed.
    assert!(text(&listed.stdout).contains("lab_ana"));
    let problem = text(&listed.stderr);
    assert!(
        problem.contains("error[profile.unknown_package]"),
        "{problem}"
    );
    assert!(
        problem.contains("profile `cobot` records robot package `cobot`"),
        "{problem}"
    );
    assert!(problem.contains("profiles/cobot.toml"), "{problem}");
    assert!(kapto(&folder, &["task", "list"]).status.success());
    let missing = kapto(&folder, &["profile", "show", "lab_ben"]);
    assert!(text(&missing.stderr).contains(
        "error[profile.not_found]: No such profile: `lab_ben`; there are: cobot, lab_ana"
    ));

    std::fs::write(folder.join(bad_task.0), bad_task.1).unwrap();
    let invalid = kapto(&folder, &["config", "validate"]);
    assert!(!invalid.status.success());
    let problems = text(&invalid.stderr);
    assert!(problems.contains("profile.unknown_package"), "{problems}");
    assert!(problems.contains("tasks/broken.toml"), "{problems}");
    assert!(
        problems.contains("2 configuration problems found"),
        "{problems}"
    );
}
