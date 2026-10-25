// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask ci`: formatting, lints, release builds without the
//! development features, dependency policy, generated pages, links and
//! tests.
//!
//! All selected steps run even if an earlier one fails, so one run shows every
//! problem. The command fails if any step failed.

use std::ffi::OsString;
use std::process::{Command, ExitCode};

use crate::{agents, links, reference, repo_root};

/// A group of checks. CI runs `check` and `test` as separate jobs so the fast
/// static checks report first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Group {
    /// Formatting, lints, dependency policy, generated pages and links.
    Check,
    /// Unit, integration and documentation tests.
    Test,
    /// Everything.
    All,
}

/// A cargo plugin that a step needs, and how to install it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Tool {
    /// The cargo subcommand the plugin provides, e.g. `nextest`.
    pub(crate) subcommand: &'static str,
    /// The command that installs it.
    pub(crate) install: &'static str,
}

/// What a step does.
pub(crate) enum Action {
    /// Runs `cargo` with these arguments. `tool` is `None` when the
    /// subcommand ships with cargo.
    Cargo {
        args: &'static [&'static str],
        tool: Option<Tool>,
    },
    /// Runs a check inside xtask; returns whether it passed and prints its
    /// own findings.
    Internal(fn() -> bool),
}

/// One check.
pub(crate) struct Step {
    pub(crate) name: &'static str,
    pub(crate) action: Action,
}

const FORMAT: Step = Step {
    name: "format",
    action: Action::Cargo {
        args: &["fmt", "--all", "--", "--check"],
        tool: None,
    },
};

// Each crate with its default features: the `ros2` feature needs ROS 2 to
// build, so `cargo xtask ros2` checks it in the ROS 2 image instead.
const LINTS: Step = Step {
    name: "lints",
    action: Action::Cargo {
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "--deny",
            "warnings",
        ],
        tool: None,
    },
};

// What only a development build has: the spans' subscriber and its trace,
// and the failpoints.
const DEVELOPMENT_LINTS: Step = Step {
    name: "development lints",
    action: Action::Cargo {
        args: &[
            "clippy",
            "--package",
            "kapto-cli",
            "--features",
            "profiling,failpoints",
            "--all-targets",
            "--",
            "--deny",
            "warnings",
        ],
        tool: None,
    },
};

const RELEASE_WITHOUT_DEVELOPMENT: Step = Step {
    name: "release without development features",
    action: Action::Internal(release_is_without_development_features),
};

const DEPENDENCY_POLICY: Step = Step {
    name: "dependency policy",
    action: Action::Cargo {
        args: &["deny", "check"],
        tool: Some(Tool {
            subcommand: "deny",
            install: "cargo install --locked cargo-deny",
        }),
    },
};

const GENERATED_PAGES: Step = Step {
    name: "generated pages",
    action: Action::Internal(generated_pages_are_current),
};

const LINKS: Step = Step {
    name: "links",
    action: Action::Internal(links_resolve),
};

const AGENTS_FILE: Step = Step {
    name: "agents file",
    action: Action::Internal(agents_file_is_current),
};

const TESTS: Step = Step {
    name: "tests",
    action: Action::Cargo {
        // An empty test suite is not an error while crates are still empty.
        args: &["nextest", "run", "--workspace", "--no-tests=pass"],
        tool: Some(Tool {
            subcommand: "nextest",
            install: "cargo install --locked cargo-nextest",
        }),
    },
};

// nextest doesn't run documentation tests, so they get their own step.
// The trace a profiling build writes, and what capture does at its
// failpoints.
const DEVELOPMENT_TESTS: Step = Step {
    name: "development tests",
    action: Action::Cargo {
        args: &[
            "nextest",
            "run",
            "--package",
            "kapto-cli",
            "--features",
            "profiling,failpoints",
            "--no-tests=pass",
            "-E",
            "test(trace::) | test(closing::)",
        ],
        tool: Some(Tool {
            subcommand: "nextest",
            install: "cargo install --locked cargo-nextest",
        }),
    },
};

const DOC_TESTS: Step = Step {
    name: "doc tests",
    action: Action::Cargo {
        args: &["test", "--workspace", "--doc"],
        tool: None,
    },
};

/// The steps a group runs, in order.
pub(crate) fn steps(group: Group) -> Vec<&'static Step> {
    let check = [
        &FORMAT,
        &LINTS,
        &DEVELOPMENT_LINTS,
        &RELEASE_WITHOUT_DEVELOPMENT,
        &DEPENDENCY_POLICY,
        &GENERATED_PAGES,
        &LINKS,
        &AGENTS_FILE,
    ];
    let test = [&TESTS, &DEVELOPMENT_TESTS, &DOC_TESTS];
    match group {
        Group::Check => check.to_vec(),
        Group::Test => test.to_vec(),
        Group::All => check.into_iter().chain(test).collect(),
    }
}

/// The message shown when a step's cargo plugin isn't installed.
pub(crate) fn missing_tool_message(step: &Step, tool: &Tool) -> String {
    format!(
        "{}: `cargo {}` is not installed. Install it with: {}",
        step.name, tool.subcommand, tool.install
    )
}

/// Runs the group's steps and reports a summary.
pub(crate) fn run(group: Group) -> ExitCode {
    // Use the same cargo that runs xtask, so the pinned toolchain applies.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut results = Vec::new();

    for step in steps(group) {
        let passed = match &step.action {
            Action::Cargo { args, tool } => {
                if let Some(tool) = tool
                    && !is_installed(&cargo, tool)
                {
                    eprintln!("{}", missing_tool_message(step, tool));
                    false
                } else {
                    println!("==> {}: cargo {}", step.name, args.join(" "));
                    Command::new(&cargo)
                        .args(*args)
                        .status()
                        .is_ok_and(|status| status.success())
                }
            }
            Action::Internal(check) => {
                println!("==> {}", step.name);
                check()
            }
        };
        results.push((step.name, passed));
    }

    println!("\nci summary");
    for (name, passed) in &results {
        println!("  {:<6} {name}", if *passed { "ok" } else { "FAILED" });
    }
    if results.iter().all(|(_, passed)| *passed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn is_installed(cargo: &OsString, tool: &Tool) -> bool {
    Command::new(cargo)
        .args([tool.subcommand, "--version"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn generated_pages_are_current() -> bool {
    let stale = reference::stale_pages(&repo_root());
    for path in &stale {
        eprintln!("{path} is out of date. Run `cargo xtask gen` and commit the result.");
    }
    stale.is_empty()
}

fn agents_file_is_current() -> bool {
    let problems = agents::problems(&repo_root());
    for problem in &problems {
        eprintln!("{problem}");
    }
    problems.is_empty()
}

/// Whether `kapto` as built by default and by the Dockerfile leaves the
/// `profiling` and `failpoints` features out: release binaries and images
/// never have them.
fn release_is_without_development_features() -> bool {
    let root = repo_root();
    let tree = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--package",
            "kapto-cli",
            "--edges",
            "features",
            "--prefix",
            "none",
        ])
        .current_dir(&root)
        .output();
    let tree = match tree {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => {
            eprintln!("cargo tree failed");
            return false;
        }
    };
    let dockerfile = std::fs::read_to_string(root.join("docker/Dockerfile")).unwrap_or_default();
    let found = development_features_enabled(&tree, &dockerfile);
    for place in &found {
        eprintln!("{place}, which release builds never have");
    }
    found.is_empty()
}

/// The features only development builds may have.
const DEVELOPMENT_FEATURES: [&str; 2] = ["profiling", "failpoints"];

/// Where a development feature is enabled: in the default build's feature
/// tree, or in a Dockerfile line building with features.
fn development_features_enabled(tree: &str, dockerfile: &str) -> Vec<String> {
    let mut found = Vec::new();
    for feature in DEVELOPMENT_FEATURES {
        let in_tree = format!("feature \"{feature}\"");
        if tree.lines().any(|line| line.contains(&in_tree)) {
            found.push(format!(
                "the default build of kapto-cli enables `{feature}`"
            ));
        }
        for (number, line) in dockerfile.lines().enumerate() {
            if line.contains("--features") && line.contains(feature) {
                found.push(format!(
                    "docker/Dockerfile:{} enables `{feature}`",
                    number + 1
                ));
            }
        }
    }
    found
}

fn links_resolve() -> bool {
    let broken = links::broken_links(&repo_root());
    for link in &broken {
        eprintln!("broken link: {link}");
    }
    broken.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(group: Group) -> Vec<&'static str> {
        steps(group).iter().map(|step| step.name).collect()
    }

    fn cargo_args(step: &Step) -> &'static [&'static str] {
        match step.action {
            Action::Cargo { args, .. } => args,
            Action::Internal(_) => &[],
        }
    }

    #[test]
    fn check_group_runs_static_checks_without_tests() {
        assert_eq!(
            names(Group::Check),
            [
                "format",
                "lints",
                "development lints",
                "release without development features",
                "dependency policy",
                "generated pages",
                "links",
                "agents file"
            ]
        );
    }

    #[test]
    fn test_group_runs_unit_and_doc_tests_only() {
        assert_eq!(
            names(Group::Test),
            ["tests", "development tests", "doc tests"]
        );
    }

    #[test]
    fn all_group_runs_static_checks_before_tests() {
        assert_eq!(
            names(Group::All),
            [
                "format",
                "lints",
                "development lints",
                "release without development features",
                "dependency policy",
                "generated pages",
                "links",
                "agents file",
                "tests",
                "development tests",
                "doc tests"
            ]
        );
    }

    #[test]
    fn a_release_build_enabling_a_development_feature_is_found() {
        let tree = "kapto-cli feature \"default\"\nkapto-core feature \"default\"\n";
        let dockerfile = "RUN cargo build --release --locked --features ros2\n";
        assert_eq!(
            development_features_enabled(tree, dockerfile),
            Vec::<String>::new()
        );
        let tree = format!("{tree}kapto-core feature \"profiling\"\n");
        let dockerfile = format!("{dockerfile}RUN cargo build --release --features failpoints\n");
        assert_eq!(
            development_features_enabled(&tree, &dockerfile),
            [
                "the default build of kapto-cli enables `profiling`",
                "docker/Dockerfile:2 enables `failpoints`"
            ]
        );
    }

    #[test]
    fn lints_treat_warnings_as_errors() {
        assert!(cargo_args(&LINTS).ends_with(&["--deny", "warnings"]));
    }

    #[test]
    fn missing_tool_message_names_the_install_command() {
        let Action::Cargo {
            tool: Some(tool), ..
        } = &TESTS.action
        else {
            panic!("the test step needs nextest");
        };
        assert_eq!(
            missing_tool_message(&TESTS, tool),
            "tests: `cargo nextest` is not installed. Install it with: \
             cargo install --locked cargo-nextest"
        );
    }

    #[test]
    fn every_plugin_step_says_how_to_install_its_plugin() {
        for step in steps(Group::All) {
            if let Action::Cargo {
                tool: Some(tool), ..
            } = &step.action
            {
                assert!(
                    tool.install
                        .ends_with(&format!("cargo-{}", tool.subcommand)),
                    "{} has a mismatched install command",
                    step.name
                );
            }
        }
    }
}
