// SPDX-License-Identifier: Apache-2.0

//! Checks that every relative link in the repository's Markdown files points
//! at a file or directory that exists. Links to web pages and anchors within
//! a page aren't checked.

use std::path::{Path, PathBuf};

/// Directories that never contain documentation.
const SKIPPED_DIRS: &[&str] = &["target", ".git", "node_modules"];

/// Every broken link, as `file:line: link`.
pub(crate) fn broken_links(root: &Path) -> Vec<String> {
    let mut broken = Vec::new();
    for file in markdown_files(root) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let shown = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .display()
            .to_string();
        for (line, target) in relative_links(&text) {
            let resolved = file.parent().unwrap_or(root).join(&target);
            if !resolved.exists() {
                broken.push(format!("{shown}:{line}: {target}"));
            }
        }
    }
    broken
}

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if !SKIPPED_DIRS.iter().any(|skipped| name == *skipped) {
                files.extend(markdown_files(&path));
            }
        } else if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// Relative link targets with their 1-based line numbers, ignoring fenced
/// code blocks, web links and anchors.
fn relative_links(text: &str) -> Vec<(usize, String)> {
    let mut links = Vec::new();
    let mut in_code_block = false;
    for (index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find("](") {
            let after = &rest[start + 2..];
            let Some(end) = after.find(')') else {
                break;
            };
            let target = after[..end].split('#').next().unwrap_or_default().trim();
            let external = target.contains("://") || target.starts_with("mailto:");
            if !target.is_empty() && !external {
                links.push((index + 1, target.to_owned()));
            }
            rest = &after[end..];
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_relative_links_with_their_lines() {
        let text = "intro\nsee [a](a.md) and [b](dir/b.md#part)\n";
        assert_eq!(
            relative_links(text),
            [(2, "a.md".to_owned()), (2, "dir/b.md".to_owned())]
        );
    }

    #[test]
    fn ignores_web_links_anchors_and_code_blocks() {
        let text = "[w](https://example.com) [m](mailto:a@b.c) [x](#section)\n\
                    ```\n[c](not-checked.md)\n```\n";
        assert_eq!(relative_links(text), []);
    }

    #[test]
    fn repository_has_no_broken_links() {
        assert_eq!(broken_links(&crate::repo_root()), Vec::<String>::new());
    }
}
