// SPDX-License-Identifier: Apache-2.0

//! Checks that AGENTS.md still describes the repository: the repository map
//! matches the crates and tools that exist, and every command it quotes
//! parses and every repository path it quotes exists, as in CONTRIBUTING.md,
//! which lists every command.
//!
//! Anything on a line that says "planned" is skipped, so the file can describe
//! work that hasn't been built yet as long as it says so.

use std::path::Path;

use clap::CommandFactory;
use clap::error::ErrorKind;

/// Repository paths are recognised by these prefixes or names.
const PATH_PREFIXES: &[&str] = &["crates/", "tools/", "docs/", ".github/", ".cargo/"];
const ROOT_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "deny.toml",
    "clippy.toml",
    "rust-toolchain.toml",
    "README.md",
    "CONTRIBUTING.md",
    "LICENSE",
    "NOTICE",
];

/// Every inconsistency between AGENTS.md and the repository.
pub(crate) fn problems(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join("AGENTS.md")) else {
        return vec!["AGENTS.md is missing".to_owned()];
    };
    let mut problems = Vec::new();
    problems.extend(map_problems(root, &text));
    problems.extend(command_problems("AGENTS.md", &text));
    problems.extend(path_problems(root, "AGENTS.md", &text));
    match std::fs::read_to_string(root.join("CONTRIBUTING.md")) {
        Ok(text) => {
            problems.extend(command_problems("CONTRIBUTING.md", &text));
            problems.extend(path_problems(root, "CONTRIBUTING.md", &text));
        }
        Err(_) => problems.push("CONTRIBUTING.md is missing".to_owned()),
    }
    problems
}

/// Lines outside fenced code blocks, numbered from 1, that aren't marked
/// as planned.
fn current_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut in_code_block = false;
    text.lines().enumerate().filter_map(move |(index, line)| {
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            return None;
        }
        let planned = line.to_lowercase().contains("planned");
        (!in_code_block && !planned).then_some((index + 1, line))
    })
}

/// Text between backticks.
fn quoted(line: &str) -> impl Iterator<Item = &str> {
    line.split('`').skip(1).step_by(2)
}

/// The repository map lists every crate and tool, and every path it lists
/// exists (unless marked planned).
fn map_problems(root: &Path, text: &str) -> Vec<String> {
    let Some(start) = text.find("## 4. Repository map") else {
        return vec!["AGENTS.md: the \"Repository map\" section is missing".to_owned()];
    };
    let section = &text[start..];
    let section = section.find("\n---").map_or(section, |end| &section[..end]);

    let mut problems = Vec::new();
    let mut mapped = Vec::new();
    for row in section.lines().filter(|line| line.starts_with("| `")) {
        let first_cell = row.split('|').nth(1).unwrap_or_default();
        let planned = row.to_lowercase().contains("planned");
        for path in quoted(first_cell) {
            mapped.push(path.trim_end_matches('/').to_owned());
            if !planned && !root.join(path).exists() {
                problems.push(format!(
                    "AGENTS.md repository map: `{path}` doesn't exist (mark the row *(planned)* if it is)"
                ));
            }
        }
    }
    for dir in ["crates", "tools"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            let path = format!("{dir}/{}", entry.file_name().to_string_lossy());
            if !mapped.contains(&path) {
                problems.push(format!(
                    "AGENTS.md repository map: `{path}` exists but isn't listed"
                ));
            }
        }
    }
    problems
}

/// Every quoted `cargo xtask …` and `kapto …` command parses with the real
/// command-line definitions.
fn command_problems(file: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (line, content) in current_lines(text) {
        for command in quoted(content) {
            let words: Vec<&str> = command.split_whitespace().collect();
            let parsed = match words.as_slice() {
                ["cargo", "xtask", rest @ ..] => crate::Cli::command()
                    .try_get_matches_from(
                        std::iter::once("cargo xtask").chain(rest.iter().copied()),
                    )
                    .map(|_| ()),
                ["kapto", ..] => kapto_cli::command()
                    .try_get_matches_from(words.iter().copied())
                    .map(|_| ()),
                _ => continue,
            };
            // A bare tool or group name (`kapto config`) only lacks a
            // subcommand; the words themselves exist.
            let incomplete = |error: &clap::Error| {
                matches!(
                    error.kind(),
                    ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                        | ErrorKind::MissingSubcommand
                )
            };
            if let Err(error) = &parsed
                && !incomplete(error)
            {
                let reason = error.to_string();
                let reason = reason.lines().next().unwrap_or_default();
                problems.push(format!(
                    "{file}:{line}: `{command}` doesn't parse: {reason}"
                ));
            }
        }
    }
    problems
}

/// Every quoted repository path exists.
fn path_problems(root: &Path, file: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (line, content) in current_lines(text) {
        for path in quoted(content) {
            let looks_like_path = !path.contains([' ', '<', '*'])
                && (PATH_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
                    || ROOT_FILES.contains(&path));
            if looks_like_path && !root.join(path).exists() {
                problems.push(format!("{file}:{line}: `{path}` doesn't exist"));
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_text_is_what_sits_between_backticks() {
        let found: Vec<&str> = quoted("run `cargo xtask ci` or `kapto config show` now").collect();
        assert_eq!(found, ["cargo xtask ci", "kapto config show"]);
    }

    #[test]
    fn lines_marked_planned_and_code_blocks_are_skipped() {
        let text = "a `x`\nb `y` *(planned)*\n```\nc `z`\n```\nd\n";
        let lines: Vec<usize> = current_lines(text).map(|(line, _)| line).collect();
        assert_eq!(lines, [1, 6]);
    }

    #[test]
    fn commands_that_do_not_exist_are_reported() {
        let text = "| Lint | `cargo xtask lint` |\n| Show | `kapto config shw` |\n";
        let problems = command_problems("AGENTS.md", text);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("AGENTS.md:1: `cargo xtask lint` doesn't parse"));
        assert!(problems[1].starts_with("AGENTS.md:2: `kapto config shw` doesn't parse"));
    }

    #[test]
    fn tool_and_group_names_on_their_own_are_accepted() {
        assert_eq!(
            command_problems("AGENTS.md", "`kapto` `kapto config` `cargo xtask`"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unknown_flags_and_values_are_reported() {
        let problems = command_problems(
            "AGENTS.md",
            "`kapto config show --prot 1` `cargo xtask ci everything`",
        );
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn real_commands_parse() {
        let text =
            "`cargo xtask ci check` `cargo xtask gen --check` `kapto config show --port 7701`";
        assert_eq!(command_problems("AGENTS.md", text), Vec::<String>::new());
    }

    #[test]
    fn missing_repository_paths_are_reported_unless_planned() {
        let root = crate::repo_root();
        let text = "see `crates/kapto-nowhere/src/lib.rs`\nlater `docs/nowhere.md` *(planned)*\n`/config` and `ui/` aren't checked\n";
        assert_eq!(
            path_problems(&root, "AGENTS.md", text),
            ["AGENTS.md:1: `crates/kapto-nowhere/src/lib.rs` doesn't exist"]
        );
    }

    #[test]
    fn the_repository_map_must_list_every_crate() {
        let root = crate::repo_root();
        let text = "## 4. Repository map\n\n| Path | Purpose |\n|---|---|\n| `crates/kapto-core` | Shared types |\n| `ui/` | Web UI *(planned)* |\n\n---\n";
        let problems = map_problems(&root, text);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("`crates/kapto-cli` exists but isn't listed"))
        );
        assert!(!problems.iter().any(|problem| problem.contains("ui")));
    }

    #[test]
    fn agents_file_matches_the_repository() {
        assert_eq!(problems(&crate::repo_root()), Vec::<String>::new());
    }
}
