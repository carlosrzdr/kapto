// SPDX-License-Identifier: Apache-2.0

//! The guide to controlling recording from other programs: every example in
//! it runs against a real `kapto serve`, as written except for the API's
//! port, so the guide can't drift from what Kapto does.
//!
//! The curl examples need `curl` and `sh`, the Python one `python3`; the
//! Rust one is compiled into this test from `guide/record_episode.rs`, and
//! the guide must show that file exactly.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

#[allow(
    dead_code,
    unreachable_pub,
    reason = "the example is a whole program; the test calls only `record`"
)]
#[path = "guide/record_episode.rs"]
mod example;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use common::root;
use common::serve::Serve;
use kapto_testkit::scratch;

/// Where the examples address the API.
const DOCUMENTED_API: &str = "http://127.0.0.1:7700/api";

/// One fenced code block: its language and its text.
struct Block {
    language: String,
    code: String,
}

fn guide() -> Vec<Block> {
    let text = std::fs::read_to_string(root().join("docs/guides/control-recording.md")).unwrap();
    let mut blocks = Vec::new();
    let mut open: Option<Block> = None;
    for line in text.lines() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(language)) => {
                open = Some(Block {
                    language: language.to_owned(),
                    code: String::new(),
                });
            }
            (Some(_), Some("")) => blocks.extend(open.take()),
            (Some(block), _) => {
                block.code.push_str(line);
                block.code.push('\n');
            }
            (None, None) => {}
        }
    }
    blocks
}

/// Runs `program` with `input` on its standard input; fails the test with
/// what it printed unless it succeeds.
fn run(program: &mut Command, input: &str) -> String {
    let mut child = program
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("can't run {program:?}, which the guide uses: {error}"));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let Output {
        status,
        stdout,
        stderr,
    } = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    assert!(
        status.success(),
        "{input}\nfailed: {status}\n{stdout}{}",
        String::from_utf8_lossy(&stderr)
    );
    stdout
}

#[test]
fn every_example_in_the_api_guide_works_against_a_running_serve() {
    let folder = scratch("guide");
    let serve = Serve::start(&folder, &root().join("packages/arm7/mapping.toml"));
    serve.wait_for("Kapto is running: ", 1);
    let api = serve.api();
    let blocks = guide();

    // The curl examples, in order; a documented error answer is what Kapto
    // answered.
    let mut curl = 0;
    for (index, block) in blocks.iter().enumerate() {
        if block.language != "sh" || !block.code.contains("curl") {
            continue;
        }
        curl += 1;
        let output = run(
            Command::new("sh").arg("-e"),
            &block.code.replace(DOCUMENTED_API, &api),
        );
        if let Some(answer) = blocks.get(index + 1)
            && answer.language == "text"
            && answer.code.starts_with("{\"code\"")
        {
            assert_eq!(output.trim(), answer.code.trim());
        }
    }
    assert!(curl >= 4, "only {curl} curl examples found");

    let python: Vec<&Block> = blocks
        .iter()
        .filter(|block| block.language == "python")
        .collect();
    assert_eq!(python.len(), 1);
    let output = run(
        Command::new("python3").args(["-I", "-"]),
        &python[0].code.replace(DOCUMENTED_API, &api),
    );
    assert!(output.contains("Recorded episode "), "{output}");

    let rust: Vec<&Block> = blocks
        .iter()
        .filter(|block| block.language == "rust")
        .collect();
    assert_eq!(rust.len(), 1);
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guide/record_episode.rs"),
    )
    .unwrap();
    let shown = source
        .strip_prefix("// SPDX-License-Identifier: Apache-2.0\n\n")
        .unwrap();
    assert_eq!(
        rust[0].code, shown,
        "the guide's Rust example isn't tests/guide/record_episode.rs"
    );
    example::record(&api, Duration::from_secs(1)).unwrap();

    // One episode each from curl, Python and Rust; the aborted one is gone.
    let (code, body) = serve.http("GET", "/api/episodes", None);
    assert_eq!(code, 200);
    assert_eq!(body.matches("\"id\":").count(), 3, "{body}");
}
