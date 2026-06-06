// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask api-diff`: compares the API's OpenAPI document with the one
//! committed at an earlier revision (by default `main`) and fails on any
//! change that could break a client, such as a removed endpoint or field.
//! Within one API version the API only gains endpoints and fields; a new
//! version may change anything.
//!
//! The comparison is oasdiff's, run in Docker from the image pinned below,
//! so the host needs nothing but Docker.

use std::path::Path;
use std::process::{Command, ExitCode};

use serde_json::Value;

/// oasdiff v1.32.1.
const OASDIFF_IMAGE: &str =
    "tufin/oasdiff@sha256:3b14fe0112e5d1bf862f91ab234a4bcd161a3f399e98f8b0b665ce70857694ac";

/// Where the document is committed, as `cargo xtask gen` writes it.
const DOCUMENT: &str = "docs/reference/openapi.json";

pub(crate) fn run(root: &Path, base: &str) -> ExitCode {
    let current = kapto_server::api::openapi_json();
    let Some(earlier) = committed(root, base) else {
        println!("No {DOCUMENT} at {base}: nothing to compare with.");
        return ExitCode::SUCCESS;
    };
    match comparison(&earlier, &current) {
        Comparison::NewVersion { from, to } => {
            println!("The API version changed from {from} to {to}: breaking changes are allowed.");
            ExitCode::SUCCESS
        }
        Comparison::Unreadable => {
            eprintln!("error: the OpenAPI document at {base} isn't JSON");
            ExitCode::FAILURE
        }
        Comparison::SameVersion => {
            println!("Comparing the API with {base} for changes that could break a client");
            match breaking(&earlier, &current) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => {
                    eprintln!(
                        "error: these changes could break clients of API version {}. \
                         Add fields and endpoints instead of changing or removing them, \
                         or raise the API version.",
                        kapto_server::api::API_VERSION
                    );
                    ExitCode::FAILURE
                }
                Err(error) => {
                    eprintln!("error: can't run oasdiff in Docker: {error}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// The document committed at `revision`, if there is one.
fn committed(root: &Path, revision: &str) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["show", &format!("{revision}:{DOCUMENT}")])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Debug, PartialEq, Eq)]
enum Comparison {
    /// Both describe the same API version: it may only have grown.
    SameVersion,
    /// The version changed, so anything may have.
    NewVersion { from: String, to: String },
    /// The earlier document can't be read.
    Unreadable,
}

fn comparison(earlier: &str, current: &str) -> Comparison {
    let version = |document: &str| {
        serde_json::from_str::<Value>(document)
            .ok()
            .and_then(|document| document["info"]["version"].as_str().map(str::to_owned))
    };
    match (version(earlier), version(current)) {
        (Some(from), Some(to)) if from != to => Comparison::NewVersion { from, to },
        (Some(_), Some(_)) => Comparison::SameVersion,
        _ => Comparison::Unreadable,
    }
}

/// Runs oasdiff on the two documents; `Ok(true)` if nothing breaks.
/// oasdiff prints what it found.
fn breaking(earlier: &str, current: &str) -> std::io::Result<bool> {
    let folder = std::env::temp_dir().join(format!("kapto-api-diff-{}", std::process::id()));
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("earlier.json"), earlier)?;
    std::fs::write(folder.join("current.json"), current)?;
    let status = Command::new("docker")
        .args(["run", "--rm", "--network", "none", "-v"])
        .arg(format!("{}:/specs:ro", folder.display()))
        .args([OASDIFF_IMAGE, "breaking", "/specs/earlier.json"])
        .args(["/specs/current.json", "--fail-on", "ERR"])
        .status();
    let _ = std::fs::remove_dir_all(&folder);
    Ok(status?.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(version: &str) -> String {
        format!(
            r#"{{"openapi":"3.1.0","info":{{"title":"t","version":"{version}"}},"paths":{{}}}}"#
        )
    }

    #[test]
    fn documents_of_the_same_version_are_compared_and_a_new_version_may_change_anything() {
        assert_eq!(
            comparison(&document("1"), &document("1")),
            Comparison::SameVersion
        );
        assert_eq!(
            comparison(&document("1"), &document("2")),
            Comparison::NewVersion {
                from: "1".into(),
                to: "2".into()
            }
        );
        assert_eq!(
            comparison("not json", &document("1")),
            Comparison::Unreadable
        );
    }

    /// The API's document with `change` made to it.
    fn changed(change: impl FnOnce(&mut Value)) -> String {
        let mut document: Value = serde_json::from_str(&kapto_server::api::openapi_json()).unwrap();
        change(&mut document);
        document.to_string()
    }

    // Runs oasdiff in Docker: `cargo test -p xtask -- --ignored oasdiff`.
    #[test]
    #[ignore = "needs Docker"]
    fn oasdiff_finds_a_removed_field_or_endpoint_and_accepts_an_added_field() {
        let current = kapto_server::api::openapi_json();
        let without_field = changed(|document| {
            let summary = &mut document["components"]["schemas"]["EpisodeSummary"];
            summary["properties"]
                .as_object_mut()
                .unwrap()
                .remove("kapto_version");
            summary["required"]
                .as_array_mut()
                .unwrap()
                .retain(|field| field != "kapto_version");
        });
        let without_endpoint = changed(|document| {
            document["paths"]
                .as_object_mut()
                .unwrap()
                .remove("/api/episodes/abort");
        });
        let with_field = changed(|document| {
            document["components"]["schemas"]["Version"]["properties"]["commit"] =
                serde_json::json!({"type": "string"});
        });
        assert!(!breaking(&current, &without_field).unwrap());
        assert!(!breaking(&current, &without_endpoint).unwrap());
        assert!(breaking(&current, &with_field).unwrap());
    }
}
