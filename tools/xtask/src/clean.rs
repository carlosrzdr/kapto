// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask clean`: shows what the build folder (`target/`) holds and
//! removes what is cheap to build again, so a laptop's disk doesn't fill up
//! with builds nobody needs any more.
//!
//! The third-party dependencies are most of the build time; Kapto's own
//! crates, tests and tools are most of the space. Removing only those keeps
//! the next build to a minute or two.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

/// What a build output is, for the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// Kapto's crates, tests and tools, and their incremental caches.
    Workspace,
    /// Third-party crates, built once and reused.
    Dependency,
    /// Build scripts and what they generate.
    BuildScript,
    /// Benchmark results, fingerprints and anything else.
    Other,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Kapto",
            Self::Dependency => "dependencies",
            Self::BuildScript => "build scripts",
            Self::Other => "other",
        }
    }
}

/// What to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Remove {
    /// Nothing: only report.
    Nothing,
    /// Kapto's own outputs; dependencies stay built.
    Workspace,
    /// The whole build folder.
    All,
}

pub(crate) fn run(root: &Path, remove: Remove) -> ExitCode {
    let target = root.join("target");
    let before = Usage::of(&target);
    print!("{}", before.report());
    let removed = match remove {
        Remove::Nothing => {
            println!(
                "\n`cargo xtask clean --workspace` removes Kapto's own outputs ({}); the next build takes a minute or two.\n`cargo xtask clean --all` removes everything ({}); the next build starts from scratch.",
                size(before.workspace()),
                size(before.total())
            );
            return ExitCode::SUCCESS;
        }
        Remove::Workspace => clean_workspace(&target, &before),
        Remove::All => cargo().arg("clean").status().is_ok_and(|s| s.success()),
    };
    if !removed {
        eprintln!("error: cargo clean failed");
        return ExitCode::FAILURE;
    }
    let after = Usage::of(&target);
    println!(
        "\nFreed {}; target/ now holds {}.",
        size(before.total().saturating_sub(after.total())),
        size(after.total())
    );
    ExitCode::SUCCESS
}

/// Runs `cargo clean` for every workspace package, in every profile that
/// was built.
fn clean_workspace(target: &Path, usage: &Usage) -> bool {
    let Some(packages) = workspace_packages() else {
        eprintln!("error: can't list the workspace's packages with `cargo metadata`");
        return false;
    };
    usage.profiles.keys().all(|profile| {
        let mut clean = cargo();
        clean
            .arg("clean")
            .arg("--profile")
            .arg(profile_name(profile));
        for package in &packages {
            clean.args(["--package", package]);
        }
        let cleaned = clean.status().is_ok_and(|s| s.success());
        // Cargo leaves the incremental caches of the packages it cleans.
        let _ = fs::remove_dir_all(target.join(profile).join("incremental"));
        cleaned
    })
}

/// The cargo profile whose outputs are in this folder of `target/`.
fn profile_name(folder: &str) -> &str {
    match folder {
        "debug" => "dev",
        other => other,
    }
}

fn workspace_packages() -> Option<Vec<String>> {
    let output = cargo()
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .ok()?;
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(
        metadata["packages"]
            .as_array()?
            .iter()
            .filter_map(|package| package["name"].as_str().map(str::to_owned))
            .collect(),
    )
}

/// The same cargo that runs xtask, so the pinned toolchain applies.
fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

/// Bytes in `target/`, by profile and kind, and in its other folders.
#[derive(Debug, Default)]
struct Usage {
    /// Per profile folder (`debug`, `release`, …), bytes per kind.
    profiles: BTreeMap<String, BTreeMap<Kind, u64>>,
    /// Other folders (benchmark results and the like), bytes each.
    others: BTreeMap<String, u64>,
}

impl Usage {
    fn of(target: &Path) -> Self {
        let mut usage = Self::default();
        let mut seen = HashSet::new();
        let Ok(entries) = fs::read_dir(target) else {
            return usage;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.join("deps").is_dir() {
                let kinds = usage.profiles.entry(name).or_default();
                walk(&path, &path, &mut seen, &mut |relative, bytes| {
                    *kinds.entry(classify(relative)).or_default() += bytes;
                });
            } else {
                let total = usage.others.entry(name).or_default();
                walk(&path, &path, &mut seen, &mut |_, bytes| *total += bytes);
            }
        }
        usage
    }

    fn total(&self) -> u64 {
        let profiles: u64 = self.profiles.values().flat_map(BTreeMap::values).sum();
        profiles + self.others.values().sum::<u64>()
    }

    fn workspace(&self) -> u64 {
        self.profiles
            .values()
            .filter_map(|kinds| kinds.get(&Kind::Workspace))
            .sum()
    }

    fn report(&self) -> String {
        let mut report = format!("target/ holds {}\n", size(self.total()));
        for (profile, kinds) in &self.profiles {
            let total: u64 = kinds.values().sum();
            if total == 0 {
                continue;
            }
            let parts: Vec<String> = kinds
                .iter()
                .map(|(kind, bytes)| format!("{} {}", kind.label(), size(*bytes)))
                .collect();
            let _ = writeln!(
                report,
                "  {profile:<10} {:>8}: {}",
                size(total),
                parts.join(", ")
            );
        }
        for (folder, bytes) in self.others.iter().filter(|(_, bytes)| **bytes > 0) {
            let _ = writeln!(report, "  {folder:<10} {:>8}", size(*bytes));
        }
        report
    }
}

/// Calls `found` with the path (relative to `root`) and size of every file
/// under `folder`. A file reached through several hard links (cargo links
/// each binary from `deps/` to the profile folder) is counted once.
fn walk(
    root: &Path,
    folder: &Path,
    seen: &mut HashSet<(u64, u64)>,
    found: &mut dyn FnMut(&Path, u64),
) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        if metadata.is_dir() {
            walk(root, &path, seen, found);
        } else if first_link(&metadata, seen)
            && let Ok(relative) = path.strip_prefix(root)
        {
            found(relative, metadata.len());
        }
    }
}

#[cfg(unix)]
fn first_link(metadata: &fs::Metadata, seen: &mut HashSet<(u64, u64)>) -> bool {
    use std::os::unix::fs::MetadataExt;
    seen.insert((metadata.dev(), metadata.ino()))
}

/// Elsewhere hard links can't be told apart, so each is counted.
#[cfg(not(unix))]
fn first_link(_: &fs::Metadata, _: &mut HashSet<(u64, u64)>) -> bool {
    true
}

/// What a file in a profile folder is, from its place and name.
fn classify(relative: &Path) -> Kind {
    let mut parts = relative.iter().map(|part| part.to_string_lossy());
    let first = parts.next().unwrap_or_default();
    let Some(file) = parts.next() else {
        // Directly in the profile folder: the binaries cargo links there.
        return Kind::Workspace;
    };
    match first.as_ref() {
        "incremental" => Kind::Workspace,
        "build" => Kind::BuildScript,
        "deps" => {
            let name = file.trim_start_matches("lib");
            if name.starts_with("kapto") || name.starts_with("xtask") || !is_library(&file) {
                // Only the workspace builds executables into `deps`: its
                // tests and binaries.
                Kind::Workspace
            } else {
                Kind::Dependency
            }
        }
        _ => Kind::Other,
    }
}

fn is_library(file: &str) -> bool {
    [".rlib", ".rmeta", ".so", ".dylib", ".dll", ".d"]
        .iter()
        .any(|extension| file.ends_with(extension))
}

/// Bytes, in the unit that reads best.
fn size(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a size shown to one decimal place"
    )]
    let bytes = bytes as f64;
    if bytes >= 1e9 {
        format!("{:.1} GB", bytes / 1e9)
    } else {
        format!("{:.0} MB", bytes / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kapto_outputs_are_told_apart_from_dependencies() {
        for (path, kind) in [
            ("deps/libkapto_core-0123abcd.rlib", Kind::Workspace),
            ("deps/kapto_cli-0123abcd", Kind::Workspace),
            ("deps/scene-0123abcd", Kind::Workspace),
            ("deps/xtask-0123abcd", Kind::Workspace),
            ("kapto", Kind::Workspace),
            (
                "incremental/kapto_viz-1x/s-abc/dep-graph.bin",
                Kind::Workspace,
            ),
            ("deps/libserde-0123abcd.rlib", Kind::Dependency),
            ("deps/libserde_derive-0123abcd.so", Kind::Dependency),
            ("deps/serde-0123abcd.d", Kind::Dependency),
            ("build/ring-0123abcd/out/libring.a", Kind::BuildScript),
            (".fingerprint/serde-0123abcd/lib-serde", Kind::Other),
        ] {
            assert_eq!(classify(Path::new(path)), kind, "{path}");
        }
    }

    #[test]
    fn hard_links_to_the_same_binary_are_counted_once() {
        let target = std::env::temp_dir().join(format!("xtask-clean-{}", std::process::id()));
        let deps = target.join("debug/deps");
        fs::create_dir_all(&deps).unwrap();
        fs::write(deps.join("kapto-0123abcd"), [0; 1000]).unwrap();
        fs::hard_link(deps.join("kapto-0123abcd"), target.join("debug/kapto")).unwrap();
        fs::write(deps.join("libserde-0123abcd.rlib"), [0; 300]).unwrap();
        fs::create_dir_all(target.join("criterion")).unwrap();
        fs::write(target.join("criterion/report.json"), [0; 20]).unwrap();

        let usage = Usage::of(&target);
        fs::remove_dir_all(&target).unwrap();
        assert_eq!(usage.workspace(), 1000);
        assert_eq!(usage.total(), 1320);
        assert_eq!(usage.others["criterion"], 20);
    }

    #[test]
    fn profile_folders_name_their_profile() {
        assert_eq!(profile_name("debug"), "dev");
        assert_eq!(profile_name("release"), "release");
    }

    #[test]
    fn sizes_read_in_megabytes_below_a_gigabyte() {
        assert_eq!(size(420_000_000), "420 MB");
        assert_eq!(size(9_480_000_000), "9.5 GB");
    }
}
