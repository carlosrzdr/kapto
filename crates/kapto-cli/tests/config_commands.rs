// SPDX-License-Identifier: Apache-2.0

//! The `kapto config` commands, run as a real process with a controlled
//! environment.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::PathBuf;
use std::process::{Command, Output};

/// Runs `kapto` with only the given environment variables set.
fn kapto(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// A configuration file unique to one test, removed when dropped.
struct TempConfig(PathBuf);

impl TempConfig {
    fn new(test: &str, contents: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("kapto-cli-{}-{test}.toml", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn show_on_a_fresh_machine_lists_defaults_and_says_no_file_was_found() {
    let output = kapto(&["config", "show"], &[("HOME", "/home/u")]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.starts_with(
        "Configuration file: /home/u/.config/kapto/kapto.toml (not found, using defaults)\n"
    ));
    assert!(
        text.lines().any(|line| line
            .split_whitespace()
            .eq(["server.port", "7700", "default"])),
        "{text}"
    );
    assert!(
        text.lines().skip(3).all(|line| line.ends_with("default")),
        "{text}"
    );
}

#[test]
fn show_names_the_source_of_each_overridden_value() {
    let config = TempConfig::new("show-sources", "[server]\nport = 7701\n");
    let output = kapto(
        &[
            "config",
            "show",
            "--config",
            config.path(),
            "--packages-dir",
            "/srv/packages",
        ],
        &[("HOME", "/home/u"), ("KAPTO_VIEWER_STREAM_PORT", "9091")],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains(&format!("Configuration file: {}\n", config.path())));
    assert!(text.contains(&format!("{}:2", config.path())));
    assert!(
        text.contains("9091") && text.contains("environment variable KAPTO_VIEWER_STREAM_PORT")
    );
    assert!(text.contains("/srv/packages") && text.contains("flag --paths-packages"));
}

#[test]
fn flags_work_before_or_after_the_subcommand() {
    for args in [
        ["--port", "7705", "config", "show"],
        ["config", "show", "--port", "7705"],
    ] {
        let output = kapto(&args, &[("HOME", "/home/u")]);
        assert!(stdout(&output).contains("7705"), "{args:?}");
    }
}

#[test]
fn validate_accepts_a_correct_configuration() {
    let config = TempConfig::new("validate-ok", "[server]\nport = 7701\n");
    let output = kapto(
        &["config", "validate", "--config", config.path()],
        &[("HOME", "/home/u")],
    );
    assert!(output.status.success());
    assert!(stdout(&output).ends_with("The configuration is valid.\n"));
}

#[test]
fn validate_warns_about_a_destination_without_the_completeness_marker_but_accepts_it() {
    let config = TempConfig::new(
        "validate-marker",
        "[[upload.destination]]\nname = \"nas\"\nurl = \"s3://robot-data\"\nmarker = false\n\
         [[upload.destination]]\nname = \"cloud\"\nurl = \"gs://archive\"\n",
    );
    let output = kapto(
        &["config", "validate", "--config", config.path()],
        &[("HOME", "/home/u")],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let warnings = stderr(&output);
    assert_eq!(warnings.lines().count(), 1, "{warnings}");
    assert!(
        warnings.starts_with(&format!(
            "warning: destination `nas` doesn't write the completeness marker `_SUCCESS` ({}:4)",
            config.path()
        )),
        "{warnings}"
    );
}

#[test]
fn validate_reports_every_problem_and_exits_with_failure() {
    let config = TempConfig::new(
        "validate-bad",
        "[server]\nprot = 7701\n[capture]\ncompression = \"gzip\"\n",
    );
    let output = kapto(
        &[
            "config",
            "validate",
            "--config",
            config.path(),
            "--port",
            "0",
        ],
        &[("HOME", "/home/u")],
    );
    assert_eq!(output.status.code(), Some(1));
    let errors = stderr(&output);
    assert!(errors.contains("error[config.unknown_key]"), "{errors}");
    assert!(errors.contains("did you mean `server.port`?"));
    assert!(errors.contains("error[config.invalid_value]"));
    assert!(errors.contains("--server-port \"0\" must be"));
    assert!(
        errors.ends_with("\n3 configuration problems found.\n"),
        "{errors}"
    );
    assert_eq!(stdout(&output), "");
}

#[test]
fn the_episode_commands_report_every_problem_of_the_configuration() {
    let config = TempConfig::new(
        "episode-bad",
        "[server]\nprot = 7701\n[capture]\ncompression = \"gzip\"\n",
    );
    let output = kapto(
        &["episode", "status", "--config", config.path()],
        &[("HOME", "/home/u")],
    );
    assert_eq!(output.status.code(), Some(1));
    let errors = stderr(&output);
    assert!(errors.contains("error[config.unknown_key]"), "{errors}");
    assert!(errors.contains("error[config.invalid_value]"), "{errors}");
    // Not taken for capture not answering: it was never asked.
    assert!(!errors.contains("capture.control_unreachable"), "{errors}");
}

#[test]
fn a_config_file_named_explicitly_must_exist() {
    let output = kapto(
        &["config", "validate", "--config", "/nonexistent/kapto.toml"],
        &[("HOME", "/home/u")],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).starts_with(
        "error[config.file_not_found]: Configuration file not found: /nonexistent/kapto.toml\n"
    ));
}

#[test]
fn the_kapto_config_variable_points_at_the_file() {
    let config = TempConfig::new("env-path", "[server]\nport = 7709\n");
    let output = kapto(
        &["config", "show"],
        &[("HOME", "/home/u"), ("KAPTO_CONFIG", config.path())],
    );
    assert!(stdout(&output).contains("7709"));
}

#[test]
fn version_flag_prints_the_version() {
    let output = kapto(&["--version"], &[]);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        format!("kapto {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn show_into_a_reader_that_has_gone_fails_quietly_instead_of_panicking() {
    // As `kapto config show | head -1` once `head` has had its line.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["config", "show"])
        .env_clear()
        .env("HOME", "/home/u")
        .stdout(writer)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stderr(&output), "");
}
